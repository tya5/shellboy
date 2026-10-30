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
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct Attempts {
    pub count: u64,
    pub input_tokens: Number,
    pub cached_tokens: Number,
    pub output_tokens: Number,
    pub reasoning_tokens: Number,
    pub total_tokens: Number,
    pub duration_ms: Number,
    #[serde(
        rename = "first_known_started_at",
        serialize_with = "serialize_start_key"
    )]
    first_start_key: Option<String>,
    #[serde(
        rename = "last_known_started_at",
        serialize_with = "serialize_start_key"
    )]
    last_start_key: Option<String>,
    pub unknown_start_count: u64,
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
fn start_key(raw: Option<&str>) -> Option<String> {
    let value = DateTime::parse_from_rfc3339(raw?).ok()?;
    // Fixed-width biased seconds also order pre-epoch and expanded-year values.
    let shifted = value.timestamp() as i128 - i64::MIN as i128;
    Some(format!(
        "{shifted:020}{:010}|{}",
        value.timestamp_subsec_nanos(),
        value
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
    ))
}
fn start_value(key: Option<&str>) -> Option<&str> {
    key.and_then(|key| key.split_once('|').map(|(_, v)| v))
}
fn serialize_start_key<S: serde::Serializer>(
    key: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    start_value(key.as_deref()).serialize(serializer)
}

fn register(db: &Connection) -> rusqlite::Result<()> {
    db.create_scalar_function(
        "usage_saved_ms",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(saved_ms(
                ctx.get_raw(0).as_str().ok(),
                ctx.get_raw(1).as_str().ok(),
            ))
        },
    )?;
    db.create_scalar_function(
        "usage_saved_start",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(start_key(ctx.get_raw(0).as_str().ok())),
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
fn standard_values_sql() -> &'static str {
    "CASE WHEN a.state IN ('completed','failed','incomplete') AND json_valid(a.usage_json) THEN jsonb_extract(a.usage_json,'$.input_tokens','$.input_tokens_details.cached_tokens','$.output_tokens','$.output_tokens_details.reasoning_tokens','$.total_tokens') END"
}
fn model_sql(s: &Selection, wide: bool) -> String {
    let names = [
        "input_tokens",
        "cached_tokens",
        "output_tokens",
        "reasoning_tokens",
        "total_tokens",
        "duration_ms",
    ];
    let projected = (0..5)
        .map(|i| usage::sql_valid_integer("standard_values", &format!("$[{i}]")))
        .collect::<Vec<_>>()
        .join(",");
    let mut select = String::from("state,count(*)");
    for field in names {
        if wide {
            select.push_str(&format!(
                ",sum({field}/1000000000),sum({field}%1000000000),count(*)-count({field})"
            ));
        } else {
            select.push_str(&format!(",sum({field}),count(*)-count({field})"));
        }
    }
    select.push_str(",min(start_key),max(start_key),count(*)-count(start_key)");
    let block = if wide { ",block" } else { "" };
    format!(
        "WITH docs AS MATERIALIZED (SELECT a.rowid/65536 block,a.state,{} standard_values,CASE WHEN a.state IN ('preparing','inflight') THEN usage_saved_ms(a.started_at,?2) ELSE usage_saved_ms(a.started_at,a.ended_at) END duration_ms,CASE WHEN a.state IN ('completed','failed','incomplete') THEN usage_saved_start(a.started_at) END start_key FROM model_attempt a INDEXED BY attempts_by_turn WHERE a.turn_id IN ({}) AND a.state IN ('completed','failed','incomplete','preparing','inflight')),values_read(block,state,input_tokens,cached_tokens,output_tokens,reasoning_tokens,total_tokens,duration_ms,start_key) AS MATERIALIZED (SELECT block,state,{projected},duration_ms,start_key FROM docs) SELECT {select} FROM values_read GROUP BY state{block}",
        standard_values_sql(),
        turn_source(s)
    )
}
// Rare SQL integer overflow retries exact high/low sums in bounded rowid blocks;
// a block has at most 131,071 rows, so each i64 partial sum remains representable.
fn models(
    db: &Connection,
    s: &Selection,
    wide: bool,
    observed: &str,
    trace: &mut Trace,
) -> rusqlite::Result<BTreeMap<String, Attempts>> {
    let sql = model_sql(s, wide);
    let mut statement = db.prepare(&sql)?;
    let mut result = BTreeMap::<String, Attempts>::new();
    {
        let mut rows = statement.query(params![s.parameter, observed])?;
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
            let meta = 2 + 6 * if wide { 3 } else { 2 };
            let first: Option<String> = row.get(meta)?;
            let last: Option<String> = row.get(meta + 1)?;
            if let Some(first) = first
                && group
                    .first_start_key
                    .as_ref()
                    .is_none_or(|old| first < *old)
            {
                group.first_start_key = Some(first);
            }
            if let Some(last) = last
                && group.last_start_key.as_ref().is_none_or(|old| last > *old)
            {
                group.last_start_key = Some(last);
            }
            group.unknown_start_count += row.get::<_, i64>(meta + 2)? as u64;
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
    let observed = chrono::Utc::now().to_rfc3339();
    let mut attempts = match models(db, &s, false, &observed, trace) {
        Ok(v) => v,
        Err(rusqlite::Error::SqliteFailure(_, Some(ref message)))
            if message.contains("integer overflow") =>
        {
            models(db, &s, true, &observed, trace).map_err(error)?
        }
        Err(e) => return Err(error(e)),
    };
    let active= ["preparing","inflight"].into_iter().filter_map(|state|attempts.remove(state).map(|group|json!({"state":state,"count":group.count,"elapsed_ms":group.duration_ms.known_sum,"unknown_elapsed_count":group.duration_ms.unknown_count,"overflow":group.duration_ms.overflow}))).collect::<Vec<_>>();
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
    let shell_filter = match s.kind {
        "turn" => "s.turn_id=?1",
        "run" => "s.owner_run_id=?1",
        _ => {
            "s.owner_run_id IN (SELECT r.id FROM json_each(?1) j JOIN run r INDEXED BY run_thread_created ON r.thread_id=j.value)"
        }
    };
    // Schema12 has no terminal owner/Turn index. Keep the complete indexed scan
    // rather than dropping shells lacking a tool-call link. Active owners do seek.
    let shell_sql = format!(
        "SELECT s.rowid/65536 block,{elapsed} duration FROM shell_execution s INDEXED BY shell_terminal_time WHERE s.state IN ('exited','failed','stopped') AND {shell_filter}"
    );
    let shell = timing(db, &shell_sql, &s.parameter, None, trace).map_err(error)?;
    let active_shell_sql = format!(
        "SELECT s.rowid/65536 block,usage_saved_ms(started_at,?2) duration FROM shell_execution s INDEXED BY shell_active_owner WHERE s.state IN ('starting','running','stopping') AND {shell_filter}"
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
    let first = attempts
        .values()
        .filter_map(|v| v.first_start_key.as_deref())
        .min();
    let last = attempts
        .values()
        .filter_map(|v| v.last_start_key.as_deref())
        .max();
    let unknown_starts = attempts
        .values()
        .map(|v| v.unknown_start_count)
        .sum::<u64>();
    let own_turn_lifetime = if let Scope::Turn(id) = scope {
        let sql = "SELECT usage_saved_ms(created_at,ended_at) FROM turn WHERE id=?1";
        let mut st = db.prepare(sql).map_err(error)?;
        let value = st
            .query_row([id], |r| r.get::<_, Option<i64>>(0))
            .map_err(error)?;
        trace
            .statements
            .push((sql.into(), st.get_status(StatementStatus::VmStep) as u64));
        value
    } else {
        None
    };
    let scope_id = match scope {
        Scope::Turn(id) | Scope::Run(id) | Scope::Thread { id, .. } => id,
    };
    let active_observed = if !active.is_empty()
        || active_run.get("count").and_then(Value::as_u64).unwrap_or(0) > 0
        || active_shell
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
    {
        Some(observed)
    } else {
        None
    };
    Ok(
        json!({"scope":s.kind,"scope_id":scope_id,"thread_id":s.threads.first(),"include_children":matches!(scope,Scope::Thread{children:true,..}),"turn_lifetime_ms":own_turn_lifetime,"attempt_period":{"first_known_started_at":start_value(first),"last_known_started_at":start_value(last),"unknown_start_count":unknown_starts},"selected_threads":s.threads.len(),"coverage":{"scope_complete":s.complete,"descendants":if s.complete{"complete"}else{"thread_limit"},"summary":"not_recorded","embedding":"out_of_scope","rag":"out_of_scope","excluded_attempt_states":["cancelled"]},"model_attempts":attempts,"model_duration_ms":total_model_time,"run":run,"shell":shell,"active":{"observed_at":active_observed,"model_attempts":active,"run":active_run,"shell":active_shell}}),
    )
}

/// One saved snapshot on the owner DB worker; no provider calls, writes or receipt.
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
