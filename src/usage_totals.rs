//! Read-only totals of saved usage, shared by owner projections and scale probes.
use crate::{
    protocol::{ApiError, ErrorCode, MAX_LIST_LIMIT},
    usage,
};
use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, StatementStatus, functions::FunctionFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum Scope {
    Turn(String),
    Run(String),
    Thread { id: String, children: bool },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Number {
    pub known_sum: Option<u64>,
    pub unknown_count: u64,
    pub overflow: bool,
}
impl Number {
    fn add(&mut self, sum: Option<u128>, unknown: u64) {
        self.unknown_count = self.unknown_count.saturating_add(unknown);
        if let Some(sum) = sum {
            let sum = self.known_sum.unwrap_or(0) as u128 + sum;
            if self.overflow || sum > u64::MAX as u128 {
                self.overflow = true;
                self.known_sum = None;
            } else {
                self.known_sum = Some(sum as u64);
            }
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attempts {
    pub count: u64,
    pub input_tokens: Number,
    pub cached_tokens: Number,
    pub output_tokens: Number,
    pub reasoning_tokens: Number,
    pub total_tokens: Number,
    pub duration_ms: Number,
}
impl Attempts {
    fn fields(&mut self) -> [&mut Number; 6] {
        [
            &mut self.input_tokens,
            &mut self.cached_tokens,
            &mut self.output_tokens,
            &mut self.reasoning_tokens,
            &mut self.total_tokens,
            &mut self.duration_ms,
        ]
    }
}

#[derive(Default)]
struct Trace {
    statements: Vec<(String, u64)>,
}
fn error(e: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("usage totals read failed: {e}"))
}
fn saved_ms(start: Option<&str>, end: Option<&str>) -> Option<i64> {
    let start = DateTime::parse_from_rfc3339(start?).ok()?;
    let end = DateTime::parse_from_rfc3339(end?).ok()?;
    (end >= start).then(|| end.signed_duration_since(start).num_milliseconds())
}
fn register(db: &Connection) -> rusqlite::Result<()> {
    db.create_scalar_function(
        "usage_saved_ms",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let a: Option<String> = ctx.get(0)?;
            let b: Option<String> = ctx.get(1)?;
            Ok(saved_ms(a.as_deref(), b.as_deref()))
        },
    )
}
struct Selection {
    threads: Vec<String>,
    parameter: String,
    kind: &'static str,
    complete: bool,
}
fn selection(db: &Connection, scope: &Scope, trace: &mut Trace) -> Result<Selection, ApiError> {
    let (kind, id) = match scope {
        Scope::Turn(id) => ("turn", id),
        Scope::Run(id) => ("run", id),
        Scope::Thread { id, .. } => ("thread", id),
    };
    let sql = match kind {
        "turn" => {
            "SELECT t.id FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=?1 AND t.deleted_at IS NULL"
        }
        "run" => {
            "SELECT t.id FROM run r JOIN thread t ON t.id=r.thread_id WHERE r.id=?1 AND t.deleted_at IS NULL"
        }
        _ => "SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL",
    };
    let mut root_statement = db.prepare(sql).map_err(error)?;
    let root: String = root_statement
        .query_row([id], |r| r.get(0))
        .optional()
        .map_err(error)?
        .ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable, "Usage scope is unavailable"))?;
    trace.statements.push((
        sql.into(),
        root_statement.get_status(StatementStatus::VmStep) as u64,
    ));
    let mut threads = vec![root];
    let mut complete = true;
    if matches!(scope, Scope::Thread { children: true, .. }) {
        let mut statement=db.prepare("WITH RECURSIVE selected(id) AS (SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL UNION SELECT t.id FROM thread t JOIN selected s ON t.parent_thread_id=s.id WHERE t.deleted_at IS NULL LIMIT ?2) SELECT id FROM selected").map_err(error)?;
        threads = statement
            .query_map(params![id, MAX_LIST_LIMIT as i64 + 1], |r| r.get(0))
            .map_err(error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(error)?;
        trace.statements.push(("WITH RECURSIVE selected(id) AS (SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL UNION SELECT t.id FROM thread t JOIN selected s ON t.parent_thread_id=s.id WHERE t.deleted_at IS NULL LIMIT ?2) SELECT id FROM selected".into(), statement.get_status(StatementStatus::VmStep) as u64));
        if threads.len() > MAX_LIST_LIMIT as usize {
            threads.truncate(MAX_LIST_LIMIT as usize);
            complete = false;
        }
    }
    let parameter = if kind == "thread" {
        serde_json::to_string(&threads).expect("IDs serialize")
    } else {
        id.clone()
    };
    Ok(Selection {
        threads,
        parameter,
        kind,
        complete,
    })
}
fn turn_source(s: &Selection) -> String {
    match s.kind {
        "turn"=>"SELECT id FROM turn WHERE id=?1".into(),
        "run"=>"SELECT id FROM turn INDEXED BY turns_by_run WHERE run_id=?1".into(),
        _=>"SELECT u.id FROM json_each(?1) j JOIN turn u INDEXED BY turns_by_thread ON u.thread_id=j.value".into(),
    }
}
fn model_sql(s: &Selection, wide: bool) -> String {
    let fields = [
        "input_tokens",
        "cached_tokens",
        "output_tokens",
        "reasoning_tokens",
        "total_tokens",
        "duration_ms",
    ];
    let columns = usage::sql_projection("a.usage_json");
    let mut select = String::from("state,count(*)");
    for field in fields {
        if wide {
            select.push_str(&format!(
                ",sum({field}/1000000000),sum({field}%1000000000),count(*)-count({field})"
            ));
        } else {
            select.push_str(&format!(",sum({field}),count(*)-count({field})"));
        }
    }
    let block = if wide { ",block" } else { "" };
    format!(
        "WITH values_read(block,state,input_tokens,cached_tokens,output_tokens,reasoning_tokens,total_tokens,duration_ms) AS MATERIALIZED (SELECT a.rowid/65536 AS block,a.state,{columns},usage_saved_ms(a.started_at,a.ended_at) AS duration_ms FROM model_attempt a INDEXED BY attempts_by_turn WHERE a.turn_id IN ({turns}) AND a.state IN ('completed','failed','incomplete')) SELECT {select} FROM values_read GROUP BY state{block}",
        turns = turn_source(s)
    )
}
// Rare SQL integer overflow retries exact high/low sums in bounded rowid blocks;
// a block has at most 131,071 rows, so each i64 partial sum remains representable.
fn models(
    db: &Connection,
    s: &Selection,
    wide: bool,
    trace: &mut Trace,
) -> rusqlite::Result<BTreeMap<String, Attempts>> {
    let sql = model_sql(s, wide);
    let mut statement = db.prepare(&sql)?;
    let mut result = BTreeMap::<String, Attempts>::new();
    {
        let mut rows = statement.query([&s.parameter])?;
        while let Some(row) = rows.next()? {
            let state: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            let group = result.entry(state).or_default();
            group.count += count as u64;
            for (i, number) in group.fields().into_iter().enumerate() {
                let offset = 2 + i * if wide { 3 } else { 2 };
                let first: Option<i64> = row.get(offset)?;
                let sum = if wide {
                    first
                        .zip(row.get::<_, Option<i64>>(offset + 1)?)
                        .map(|(hi, lo)| hi as u128 * 1_000_000_000 + lo as u128)
                } else {
                    first.map(|v| v as u128)
                };
                let unknown: i64 = row.get(offset + if wide { 2 } else { 1 })?;
                number.add(sum, unknown as u64);
            }
        }
    }
    trace
        .statements
        .push((sql, statement.get_status(StatementStatus::VmStep) as u64));
    Ok(result)
}
fn timing(
    db: &Connection,
    source: &str,
    parameter: &str,
    observed: Option<&str>,
    trace: &mut Trace,
) -> rusqlite::Result<Value> {
    let mut number = Number::default();
    let mut count = 0u64;
    for wide in [false, true] {
        let sql = if wide {
            format!(
                "WITH values_read AS MATERIALIZED ({source}) SELECT count(*),sum(duration/1000000000),sum(duration%1000000000),count(*)-count(duration) FROM values_read GROUP BY block"
            )
        } else {
            format!(
                "WITH values_read AS MATERIALIZED ({source}) SELECT count(*),sum(duration),count(*)-count(duration) FROM values_read"
            )
        };
        let result = (|| -> rusqlite::Result<()> {
            let mut st = db.prepare(&sql)?;
            {
                let mut rows = if st.parameter_count() == 2 {
                    st.query(params![parameter, observed])?
                } else {
                    st.query([parameter])?
                };
                while let Some(row) = rows.next()? {
                    count += row.get::<_, i64>(0)? as u64;
                    let n: Option<i64> = row.get(1)?;
                    let sum = if wide {
                        n.zip(row.get::<_, Option<i64>>(2)?)
                            .map(|(hi, lo)| hi as u128 * 1_000_000_000 + lo as u128)
                    } else {
                        n.map(|n| n as u128)
                    };
                    number.add(sum, row.get::<_, i64>(if wide { 3 } else { 2 })? as u64);
                }
            }
            trace
                .statements
                .push((sql, st.get_status(StatementStatus::VmStep) as u64));
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(json!({"count":count,"duration_ms":number})),
            Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
                if !wide && message.contains("integer overflow") =>
            {
                number = Number::default();
                count = 0;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("wide timing aggregation returns or fails")
}
fn inspect(db: &Connection, scope: &Scope, trace: &mut Trace) -> Result<Value, ApiError> {
    register(db).map_err(error)?;
    let s = selection(db, scope, trace)?;
    let attempts = match models(db, &s, false, trace) {
        Ok(v) => v,
        Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
            if message.contains("integer overflow") =>
        {
            models(db, &s, true, trace).map_err(error)?
        }
        Err(e) => return Err(error(e)),
    };
    let active_sql = format!(
        "WITH values_read AS (SELECT state,usage_saved_ms(started_at,?2) elapsed FROM model_attempt a INDEXED BY attempts_by_turn WHERE turn_id IN ({}) AND state IN ('preparing','inflight')) SELECT state,count(*),sum(elapsed),count(*)-count(elapsed) FROM values_read GROUP BY state",
        turn_source(&s)
    );
    let observed = chrono::Utc::now().to_rfc3339();
    let active: Vec<Value> = {
        let mut st = db.prepare(&active_sql).map_err(error)?;
        let result=st.query_map(params![s.parameter,observed],|r|Ok(json!({"state":r.get::<_,String>(0)?,"count":r.get::<_,i64>(1)?,"elapsed_ms":r.get::<_,Option<i64>>(2)?,"unknown_elapsed_count":r.get::<_,i64>(3)?}))).map_err(error)?.collect::<rusqlite::Result<_>>().map_err(error)?;
        trace
            .statements
            .push((active_sql, st.get_status(StatementStatus::VmStep) as u64));
        result
    };
    let (run, active_run) = if s.kind == "turn" {
        let na = json!({"duration_ms":null,"status":"not_applicable","reason":"A parent Run lifetime is not duplicated into a Turn"});
        (na.clone(), na)
    } else {
        let filter = if s.kind == "run" {
            "r.id=?1"
        } else {
            "r.thread_id IN (SELECT value FROM json_each(?1))"
        };
        let done = format!(
            "SELECT r.rowid/65536 block,usage_saved_ms(created_at,ended_at) duration FROM run r WHERE {filter} AND state NOT IN ('live','stopping')"
        );
        let active = format!(
            "SELECT r.rowid/65536 block,usage_saved_ms(created_at,?2) duration FROM run r WHERE {filter} AND state IN ('live','stopping')"
        );
        (
            timing(db, &done, &s.parameter, None, trace).map_err(error)?,
            timing(db, &active, &s.parameter, Some(&observed), trace).map_err(error)?,
        )
    };
    let elapsed = "CASE WHEN json_type(s.shell_json,'$.completion.elapsed_ms')='integer' AND typeof(json_extract(s.shell_json,'$.completion.elapsed_ms'))='integer' AND json_extract(s.shell_json,'$.completion.elapsed_ms')>=0 THEN json_extract(s.shell_json,'$.completion.elapsed_ms') END";
    let shell_sql = format!(
        "SELECT s.rowid/65536 block,{elapsed} duration FROM shell_execution s INDEXED BY shell_terminal_time WHERE s.state IN ('exited','failed','stopped') AND s.turn_id IN ({})",
        turn_source(&s)
    );
    let shell = timing(db, &shell_sql, &s.parameter, None, trace).map_err(error)?;
    let active_shell_sql = format!(
        "SELECT s.rowid/65536 block,usage_saved_ms(started_at,?2) duration FROM shell_execution s INDEXED BY shell_active_owner WHERE s.state IN ('starting','running','stopping') AND s.turn_id IN ({})",
        turn_source(&s)
    );
    let active_shell =
        timing(db, &active_shell_sql, &s.parameter, Some(&observed), trace).map_err(error)?;
    let mut total_model_time = Number::default();
    for group in attempts.values() {
        total_model_time.add(
            group.duration_ms.known_sum.map(u128::from),
            group.duration_ms.unknown_count,
        );
        total_model_time.overflow |= group.duration_ms.overflow;
        if total_model_time.overflow {
            total_model_time.known_sum = None;
        }
    }
    Ok(
        json!({"scope":s.kind,"selected_threads":s.threads.len(),"coverage":{"complete":s.complete,"descendants":if s.complete{"complete"}else{"thread_limit"},"summary":"not_recorded","embedding":"out_of_scope","rag":"out_of_scope","excluded_attempt_states":["cancelled"]},"model_attempts":attempts,"model_duration_ms":total_model_time,"run":run,"shell":shell,"active":{"observed_at":observed,"model_attempts":active,"run":active_run,"shell":active_shell}}),
    )
}

/// One saved snapshot; no provider calls, storage mutation, receipt or file I/O.
pub fn read(db: &Connection, scope: &Scope) -> Result<Value, ApiError> {
    if db.is_autocommit() {
        let tx = db.unchecked_transaction().map_err(error)?;
        inspect(&tx, scope, &mut Trace::default())
    } else {
        inspect(db, scope, &mut Trace::default())
    }
}

#[cfg(test)]
mod tests;
