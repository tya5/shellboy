//! Bounded saved Run/Turn metadata; totals are collected separately in the same snapshot.
use crate::{
    context::ExecutionContext,
    protocol::{ApiError, Command, ErrorCode, RunDetails, TurnDetails},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
const JSON_BYTES: usize = 64 * 1024;
const OWNED_IDS: usize = 128;

// Bound each indexed state prefix BEFORE timestamp sorting. The final sort
// sees at most258/387 rows, including overflow sentinels, not an owner's entire
// lifetime. Complete small sets retain the canonical timestamp/ID order.
const OWNED_EXECUTIONS: &str = "WITH starting AS MATERIALIZED (SELECT CASE WHEN octet_length(id)<=512 THEN id ELSE '' END AS id,CASE WHEN octet_length(started_at)>512 THEN '' ELSE started_at END AS started_at,COALESCE(octet_length(id)>512 OR octet_length(started_at)>512,0) AS overflow FROM shell_execution INDEXED BY shell_active_owner WHERE owner_run_id=?1 AND state='starting' AND state IN ('starting','running','stopping') ORDER BY rowid LIMIT 129), running AS MATERIALIZED (SELECT CASE WHEN octet_length(id)<=512 THEN id ELSE '' END AS id,CASE WHEN octet_length(started_at)>512 THEN '' ELSE started_at END AS started_at,COALESCE(octet_length(id)>512 OR octet_length(started_at)>512,0) AS overflow FROM shell_execution INDEXED BY shell_active_owner WHERE owner_run_id=?1 AND state='running' AND state IN ('starting','running','stopping') ORDER BY rowid LIMIT 129), stopping AS MATERIALIZED (SELECT CASE WHEN octet_length(id)<=512 THEN id ELSE '' END AS id,CASE WHEN octet_length(started_at)>512 THEN '' ELSE started_at END AS started_at,COALESCE(octet_length(id)>512 OR octet_length(started_at)>512,0) AS overflow FROM shell_execution INDEXED BY shell_active_owner WHERE owner_run_id=?1 AND state='stopping' AND state IN ('starting','running','stopping') ORDER BY rowid LIMIT 129), candidates AS (SELECT * FROM starting UNION ALL SELECT * FROM running UNION ALL SELECT * FROM stopping) SELECT id,overflow FROM candidates ORDER BY started_at,id LIMIT 129";

const RUN_SQL: &str = "SELECT CASE WHEN octet_length(r.thread_id)>512 THEN '' ELSE r.thread_id END,
    CASE WHEN octet_length(r.state)>512 THEN '' ELSE r.state END,
    CASE WHEN octet_length(r.created_at)>512 THEN '' ELSE r.created_at END,
    CASE WHEN octet_length(r.ended_at)>512 THEN '' ELSE r.ended_at END,
    CASE WHEN octet_length(r.stop_reason)>512 THEN '' ELSE r.stop_reason END,
    CASE WHEN octet_length(u.id)>512 THEN '' ELSE u.id END,
    CASE WHEN octet_length(u.answer_node_id)>512 THEN '' ELSE u.answer_node_id END,

                CASE WHEN octet_length(u.recovery_reason_json)<=?2 THEN u.recovery_reason_json END,

                COALESCE(octet_length(u.recovery_reason_json)>?2,0)
                , (COALESCE(octet_length(r.thread_id)>512,0)
    OR COALESCE(octet_length(r.state)>512,0)
    OR COALESCE(octet_length(r.created_at)>512,0)
    OR COALESCE(octet_length(r.ended_at)>512,0)
    OR COALESCE(octet_length(r.stop_reason)>512,0)
    OR COALESCE(octet_length(u.id)>512,0)
    OR COALESCE(octet_length(u.answer_node_id)>512,0))
FROM run r LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=r.id ORDER BY created_at DESC,id DESC LIMIT 1)
                WHERE r.id=?1";

const TURN_SQL: &str =
    "SELECT CASE WHEN octet_length(u.thread_id)>512 THEN '' ELSE u.thread_id END,
    CASE WHEN octet_length(u.run_id)>512 THEN '' ELSE u.run_id END,
    CASE WHEN octet_length(u.state)>512 THEN '' ELSE u.state END,
    CASE WHEN octet_length(u.created_at)>512 THEN '' ELSE u.created_at END,
    CASE WHEN octet_length(u.ended_at)>512 THEN '' ELSE u.ended_at END,
    CASE WHEN octet_length(u.answer_node_id)>512 THEN '' ELSE u.answer_node_id END,

            CASE WHEN octet_length(u.recovery_reason_json)<=?2 THEN u.recovery_reason_json END,
    COALESCE(octet_length(u.recovery_reason_json)>?2,0)
            , (COALESCE(octet_length(u.thread_id)>512,0)
    OR COALESCE(octet_length(u.run_id)>512,0)
    OR COALESCE(octet_length(u.state)>512,0)
    OR COALESCE(octet_length(u.created_at)>512,0)
    OR COALESCE(octet_length(u.ended_at)>512,0)
    OR COALESCE(octet_length(u.answer_node_id)>512,0))
FROM turn u JOIN run r ON r.id=u.run_id WHERE u.id=?1";

struct RawRun {
    waiting_reason: Option<crate::protocol::RunWaitReason>,
    scalar_overflow: bool,
    thread: String,
    state: String,
    created: String,
    ended: Option<String>,
    stop_reason: Option<String>,
    turn: Option<String>,
    answer: Option<String>,
    reason: Option<String>,
    reason_limited: bool,
    revisit: Option<crate::protocol::RevisitSummary>,
    executions: Vec<String>,
    executions_limited: bool,
}
struct RawTurn {
    scalar_overflow: bool,
    thread: String,
    run: String,
    state: String,
    created: String,
    ended: Option<String>,
    answer: Option<String>,
    reason: Option<String>,
    reason_limited: bool,
}
pub(super) struct Raw {
    command: Command,
    body: RawBody,
    observed_at: String,
}
enum RawBody {
    Run(RawRun),
    Turn(RawTurn),
}
fn missing() -> StorageError {
    ApiError::new(
        ErrorCode::TargetUnavailable,
        "Saved Run/Turn is unavailable",
    )
    .into()
}
pub(super) fn supports(command: &Command) -> bool {
    matches!(command, Command::RunGet(_) | Command::TurnGet(_))
}
pub(super) fn read_tx(db: &Connection, command: &Command) -> Result<Raw, StorageError> {
    let id = match command {
        Command::RunGet(a) => &a.run_id,
        Command::TurnGet(a) => &a.turn_id,
        _ => {
            return Err(
                ApiError::new(ErrorCode::InvalidArgument, "Not a Run/Turn inspection").into(),
            );
        }
    };
    if id.is_empty() || id.len() > 4096 {
        return Err(
            ApiError::new(ErrorCode::InvalidArgument, "Invalid inspection identity").into(),
        );
    }
    let body = match command {
        Command::RunGet(args) => {
            let id = &args.run_id;
            // Each reason is length-gated before String materialization.
            let mut raw = db
                .query_row(RUN_SQL, rusqlite::params![id, JSON_BYTES as i64], |row| {
                    Ok(RawRun {
                        waiting_reason: None,
                        scalar_overflow: row.get(9)?,
                        thread: row.get(0)?,
                        state: row.get(1)?,

                        created: row.get(2)?,
                        ended: row.get(3)?,
                        stop_reason: row.get(4)?,
                        turn: row.get(5)?,
                        answer: row.get(6)?,
                        reason: row.get(7)?,
                        reason_limited: row.get(8)?,
                        revisit: None,
                        executions: Vec::new(),
                        executions_limited: false,
                    })
                })
                .optional()?
                .ok_or_else(missing)?;
            if !raw.scalar_overflow {
                raw.waiting_reason = crate::readback::run_wait_reason(db, &raw.thread, id)?;
            }
            raw.revisit = crate::control::revisit::summary_for_run(db, id)?;
            (raw.executions, raw.executions_limited) = owned(db, OWNED_EXECUTIONS, id)?;
            RawBody::Run(raw)
        }
        Command::TurnGet(args) => {
            let id = &args.turn_id;
            RawBody::Turn(
                db.query_row(TURN_SQL, rusqlite::params![id, JSON_BYTES as i64], |row| {
                    Ok(RawTurn {
                        scalar_overflow: row.get(8)?,
                        thread: row.get(0)?,
                        run: row.get(1)?,
                        state: row.get(2)?,

                        created: row.get(3)?,
                        ended: row.get(4)?,
                        answer: row.get(5)?,
                        reason: row.get(6)?,
                        reason_limited: row.get(7)?,
                    })
                })
                .optional()?
                .ok_or_else(missing)?,
            )
        }
        _ => unreachable!("closed allowlist checked before SQL"),
    };
    if match &body {
        RawBody::Run(raw) => raw.scalar_overflow,
        RawBody::Turn(raw) => raw.scalar_overflow,
    } {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Saved Run/Turn scalar exceeds512 bytes; use the full CLI getter",
        )
        .into());
    }
    Ok(Raw {
        command: command.clone(),
        body,
        observed_at: chrono::Utc::now().to_rfc3339(),
    })
}
fn owned(db: &Connection, sql: &str, id: &str) -> Result<(Vec<String>, bool), StorageError> {
    let mut ids = db
        .prepare(sql)?
        .query_map([id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, bool)>>>()?;
    if ids.iter().any(|(_, overflow)| *overflow) {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Saved owned identity/timestamp exceeds512 bytes",
        )
        .into());
    }
    let limited = ids.len() > OWNED_IDS;
    ids.truncate(OWNED_IDS);
    Ok((ids.into_iter().map(|(id, _)| id).collect(), limited))
}
fn limited_reason() -> Value {
    serde_json::json!({"observation":"unavailable","message":"Saved recovery reason exceeds the64KiB source allowance; inspect the original Turn","source_limit_bytes":JSON_BYTES})
}
fn decoded_reason(raw: Option<&str>, limited: bool) -> Result<Option<Value>, ApiError> {
    if limited {
        Ok(Some(limited_reason()))
    } else {
        crate::context::prepare_reason(raw)
    }
}
fn reason_hint(
    raw: Option<String>,
    limited: bool,
    language: crate::config::UiLanguage,
) -> Option<String> {
    if limited {
        Some(
            crate::ui::i18n::local!(
                language,
                "取得範囲外: 保存された復旧理由は64KiBを超えています。Turn原文で確認してください。",
                "Unavailable: the saved recovery reason exceeds 64 KiB. Inspect the original Turn."
            )
            .into(),
        )
    } else {
        raw
    }
}
fn encoded(value: impl serde::Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidState,
            "Cannot prepare saved inspection facts",
        )
    })
}

#[cfg(test)]
fn prepare(raw: Raw) -> Result<super::inspection_saved::PreparedSaved, ApiError> {
    prepare_in(raw, crate::config::UiLanguage::Ja)
}

pub(super) fn prepare_in(
    raw: Raw,
    language: crate::config::UiLanguage,
) -> Result<super::inspection_saved::PreparedSaved, ApiError> {
    let mut limited_fields = Vec::new();
    let id = match &raw.command {
        Command::RunGet(a) => &a.run_id,
        Command::TurnGet(a) => &a.turn_id,
        _ => unreachable!(),
    };
    let (thread, value) = match raw.body {
        RawBody::Run(raw) => {
            if raw.reason_limited {
                limited_fields.extend(["recovery_hint".into(), "context.recovery_reason".into()]);
            }
            if raw.executions_limited {
                limited_fields.push("owned_execution_ids".into());
            }
            let recovery_reason = decoded_reason(raw.reason.as_deref(), raw.reason_limited)?;
            let context = ExecutionContext {
                thread_id: Some(raw.thread.clone()),
                state: raw.state.clone(),
                created_at: Some(raw.created.clone()),
                ended_at: raw.ended.clone(),
                stop_reason: raw.stop_reason,
                run_id: Some(id.clone()),
                turn_id: raw.turn.clone(),
                lifetime_ms: crate::context::lifetime(&raw.created, raw.ended.as_deref()),
                recovery_reason,
                ..Default::default()
            };
            let thread = raw.thread.clone();
            let state = serde_json::from_value(serde_json::json!(raw.state))
                .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved Run state"))?;
            (
                thread,
                encoded(RunDetails {
                    waiting_reason: raw.waiting_reason,
                    usage_totals: None,
                    context,
                    run_id: id.clone(),
                    thread_id: raw.thread,
                    state,
                    turn_id: raw.turn,
                    end_message_node_id: raw.answer,
                    owned_execution_ids: raw.executions,
                    recovery_hint: reason_hint(raw.reason, raw.reason_limited, language),
                    revisit: raw.revisit,
                })?,
            )
        }
        RawBody::Turn(raw) => {
            if raw.reason_limited {
                limited_fields.extend(["recovery_hint".into(), "context.recovery_reason".into()]);
                if raw.state == "failed" {
                    limited_fields.push("context.failure_reason".into());
                }
            }
            let recovery_reason = decoded_reason(raw.reason.as_deref(), raw.reason_limited)?;
            let context = ExecutionContext {
                thread_id: Some(raw.thread.clone()),
                run_id: Some(raw.run.clone()),
                state: raw.state.clone(),
                created_at: Some(raw.created.clone()),
                ended_at: raw.ended.clone(),
                turn_id: Some(id.clone()),
                lifetime_ms: crate::context::lifetime(&raw.created, raw.ended.as_deref()),
                failure_reason: if raw.state == "failed" {
                    recovery_reason.clone()
                } else {
                    None
                },
                recovery_reason,
                ..Default::default()
            };
            let state = serde_json::from_value(serde_json::json!(raw.state))
                .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved Turn state"))?;
            let thread = raw.thread.clone();
            (
                thread,
                encoded(TurnDetails {
                    usage_totals: None,
                    context,
                    turn_id: id.clone(),
                    run_id: raw.run,
                    thread_id: raw.thread,
                    state,
                    end_message_node_id: raw.answer,
                    recovery_hint: reason_hint(raw.reason, raw.reason_limited, language),
                })?,
            )
        }
    };
    Ok(super::inspection_saved::PreparedSaved {
        value,
        thread: Some(thread),
        limited_fields,
        observed_at: raw.observed_at,
    })
}

pub(super) async fn request(
    control: &std::sync::Arc<crate::control::Control>,
    command: Command,
    stop: tokio_util::sync::CancellationToken,
    trigger: crate::usage_totals::UsageTrigger,
) -> Result<Value, ApiError> {
    command.validate()?;
    let scope = match &command {
        Command::RunGet(a) => crate::usage_totals::Scope::Run(a.run_id.clone()),
        Command::TurnGet(a) => crate::usage_totals::Scope::Turn(a.turn_id.clone()),
        _ => {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Not a Run/Turn inspection",
            ));
        }
    };
    let mut context = crate::usage_totals::read_context();
    context.trigger = trigger;
    let diagnostics = control.store.usage_diagnostics.clone();
    let (raw, totals) = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop,
        move |db| {
            let raw = read_tx(db, &command)?;
            let totals = crate::control::db::saved_usage_for_detail(
                db,
                &scope,
                context,
                diagnostics.as_ref(),
            )?;
            Ok((raw, totals))
        },
    )
    .await?;
    let language = control.store.config.ui.language;
    let mut prepared = super::presentation_work(move || prepare_in(raw, language)).await??;
    prepared.value["usage_totals"] = totals;
    prepared.value["view_observation"] =
        serde_json::json!({"observed_at":prepared.observed_at,"thread_id":prepared.thread});
    if !prepared.limited_fields.is_empty() {
        prepared.value["view_limits"] =
            serde_json::json!({"limited_fields":prepared.limited_fields});
    }
    Ok(prepared.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use rusqlite::{StatementStatus, params};
    use serde_json::json;
    fn seed(db: &Connection) {
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('foreign','Foreign','/foreign','AGENTS.md','p','m','2026-01-01T00:00:03Z'),('viewer','Viewer','/viewer','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','foreign','live','2026-01-01T00:00:00Z');
            INSERT INTO turn(id,thread_id,run_id,state,generation,created_at,recovery_reason_json)VALUES('u','foreign','r','recovery_wait',7,'2026-01-01T00:00:01Z','{\"code\":\"IoFailed\",\"message\":\"saved reason\"}');").unwrap();
        crate::control::revisit::seed_fixture(db, "foreign", "r", 3, false);
    }
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        seed(&db);
        db
    }
    fn seed_owned(db: &Connection, start: usize, end: usize) {
        for i in start..end {
            let thread = format!("child-{i}");
            let run = format!("child-run-{i}");
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'Child','/','','p','m','now')",[&thread]).unwrap();
            db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?2,'live',?3)",params![run,thread,format!("stamp-{i:05}")]).unwrap();
            db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,'r','u',?1,'running','/','{}','UTF-8','UTF-8','open',?2)",params![format!("shell-{i}"),format!("stamp-{i:05}")]).unwrap();
        }
    }
    fn steps(db: &Connection, sql: &str) -> (i32, i32) {
        let mut statement = db.prepare(sql).unwrap();
        statement
            .query_map(["r"], |_| Ok(()))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        (
            statement.get_status(StatementStatus::VmStep),
            statement.get_status(StatementStatus::FullscanStep),
        )
    }

    fn run() -> Command {
        Command::RunGet(crate::protocol::RunTarget { run_id: "r".into() })
    }
    fn turn() -> Command {
        Command::TurnGet(crate::protocol::TurnTarget {
            turn_id: "u".into(),
        })
    }
    fn canonical(db: &Connection, command: &Command) -> Value {
        match command {
            Command::RunGet(a) => {
                serde_json::to_value(crate::control::db::run_details(db, &a.run_id).unwrap())
                    .unwrap()
            }
            Command::TurnGet(a) => {
                serde_json::to_value(crate::control::db::turn_details(db, &a.turn_id).unwrap())
                    .unwrap()
            }
            _ => unreachable!(),
        }
    }
    #[test]
    fn saved_run_inspection_preserves_context_boundary_wait_and_its_removal() {
        let db = fixture();
        db.execute_batch("UPDATE turn SET state='running' WHERE id='u';
          INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('boundary','fixture','{}','hash','completed','{\"result\":{\"context_edit\":{\"action\":\"summary\"}}}','2026-10-31T12:38:20Z');
          INSERT INTO app_meta(key,value)VALUES('compact.inflight.foreign','[\"boundary\"]');").unwrap();
        let expected = canonical(&db, &run());
        let trace = Statements::new(&db);
        let raw = read_tx(&db, &run()).unwrap();
        assert_eq!(trace.count(), 6);
        drop(trace);
        let prepared = prepare(raw).unwrap();
        assert_eq!(prepared.value, expected);
        assert_eq!(prepared.value["waiting_reason"]["operation_id"], "boundary");
        assert_eq!(prepared.value["waiting_reason"]["action"], "summary");
        db.execute(
            "DELETE FROM app_meta WHERE key='compact.inflight.foreign'",
            [],
        )
        .unwrap();
        let cleared = prepare(read_tx(&db, &run()).unwrap()).unwrap();
        assert_eq!(cleared.value, canonical(&db, &run()));
        assert!(cleared.value.get("waiting_reason").is_none());
    }
    #[test]
    fn exact_saved_metadata_matches_canonical_and_uses_bounded_query_count() {
        let db = fixture();
        seed_owned(&db, 0, 8);
        for terminal in [false, true] {
            if terminal {
                db.execute_batch("UPDATE turn SET state='failed',ended_at='2026-01-01T00:00:02Z';UPDATE run SET state='completed',ended_at='2026-01-01T00:00:03Z';UPDATE thread SET deleted_at='2026-01-01T00:00:04Z' WHERE id='foreign';").unwrap();
            }
            for command in [run(), turn()] {
                let expected = canonical(&db, &command);
                let trace = Statements::new(&db);
                let raw = read_tx(&db, &command).unwrap();
                assert_eq!(
                    trace.count(),
                    if matches!(command, Command::RunGet(_)) {
                        5
                    } else {
                        1
                    }
                );
                drop(trace);
                let prepared = prepare(raw).unwrap();
                assert_eq!(prepared.value, expected);
                assert_eq!(prepared.thread.as_deref(), Some("foreign"));
                assert!(prepared.limited_fields.is_empty());
            }
        }
    }
    #[test]
    fn empty_owned_start_keeps_canonical_order() {
        let db = fixture();
        seed_owned(&db, 0, 2);
        db.execute_batch("UPDATE shell_execution SET started_at='stamp' WHERE id='shell-0';UPDATE shell_execution SET started_at='' WHERE id='shell-1';").unwrap();
        let value = prepare(read_tx(&db, &run()).unwrap()).unwrap();
        assert_eq!(
            value.value["owned_execution_ids"],
            json!(["shell-1", "shell-0"])
        );
        assert_eq!(value.value, canonical(&db, &run()));
    }
    #[test]
    fn owned_prefix_cost_is_constant_past_cap_and_small_order_matches_canonical() {
        let db = fixture();
        seed_owned(&db, 0, 8);
        assert_eq!(
            prepare(read_tx(&db, &run()).unwrap()).unwrap().value,
            canonical(&db, &run())
        );
        seed_owned(&db, 8, 150);
        let before = steps(&db, OWNED_EXECUTIONS);
        let value = prepare(read_tx(&db, &run()).unwrap()).unwrap();
        for key in ["owned_execution_ids"] {
            assert_eq!(value.value[key].as_array().unwrap().len(), 128);
            assert!(value.limited_fields.contains(&key.into()));
        }
        seed_owned(&db, 150, 1150);
        assert_eq!(steps(&db, OWNED_EXECUTIONS), before);
        for sql in [OWNED_EXECUTIONS] {
            let plans = db
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(["r"], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(plans.iter().any(|p| p.contains("USING INDEX")), "{plans:?}");
            assert!(
                !plans
                    .iter()
                    .any(|p| p == "SCAN run" || p == "SCAN shell_execution"),
                "{plans:?}"
            );
        }
    }
    #[test]
    fn latest_turn_tie_breaker_reuses_one_index_and_bounds_complete_run_query() {
        let db = fixture();
        // Force the historical layout even after a future migration extends it.
        // Only this disposable fixture changes; no migration is adopted here.
        db.execute_batch("UPDATE turn SET state='completed' WHERE id='u'; DROP INDEX activity_turn_run; DROP INDEX turns_by_run; CREATE INDEX turns_by_run ON turn(run_id,created_at);").unwrap();
        let seed_ties = |from: usize, until: usize| {
            db.execute_batch("BEGIN").unwrap();
            for i in from..until {
                db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,'foreign','r','completed','2026-01-01T00:00:04Z')",[format!("tie-{i:06}")]).unwrap();
            }
            db.execute_batch("COMMIT").unwrap();
        };
        let measure = || {
            let trace = Statements::new(&db);
            let mut statement = db.prepare(RUN_SQL).unwrap();
            let turns = statement
                .query_map(params!["r", JSON_BYTES as i64], |row| {
                    row.get::<_, Option<String>>(6)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(turns.len(), 1);
            assert_eq!(
                trace.count(),
                1,
                "measure the complete Run metadata query, not a rewritten latest-Turn surrogate"
            );
            (
                turns[0].clone(),
                [
                    statement.get_status(StatementStatus::VmStep),
                    statement.get_status(StatementStatus::Sort),
                    statement.get_status(StatementStatus::FullscanStep),
                ],
            )
        };
        let plan = || {
            db.prepare(&format!("EXPLAIN QUERY PLAN {RUN_SQL}"))
                .unwrap()
                .query_map(params!["r", JSON_BYTES as i64], |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        seed_ties(0, 32);
        let old_small = measure();
        seed_ties(32, 4096);
        let old_dense = measure();
        let expected = canonical(&db, &run());
        assert_eq!(
            prepare(read_tx(&db, &run()).unwrap()).unwrap().value,
            expected
        );
        assert_eq!(old_dense.0.as_deref(), Some("tie-004095"));
        assert!(
            old_dense.1[0] > old_small.1[0],
            "old equal-time work must be witnessed: {old_small:?}->{old_dense:?}"
        );
        assert!(
            old_dense.1[1] > 0,
            "old index leaves a tie-breaker sort: {old_dense:?}"
        );
        assert!(plan().iter().any(|part| part.contains("TEMP B-TREE")));
        db.execute_batch(
            "DROP INDEX turns_by_run; CREATE INDEX turns_by_run ON turn(run_id,created_at,id);",
        )
        .unwrap();
        let extended = measure();
        assert_eq!(extended.0, old_dense.0);
        assert_eq!(
            extended.1[1], 0,
            "the existing index extension removes the sort"
        );
        assert_eq!(extended.1[2], 0);
        assert!(extended.1[0] < old_dense.1[0]);
        assert_eq!(
            prepare(read_tx(&db, &run()).unwrap()).unwrap().value,
            expected,
            "same rows and canonical fields before/after the index replacement"
        );
        let extended_plan = plan();
        assert!(
            extended_plan
                .iter()
                .any(|part| part.contains("COVERING INDEX turns_by_run")),
            "{extended_plan:?}"
        );
        assert!(
            !extended_plan
                .iter()
                .any(|part| part.contains("TEMP B-TREE")),
            "{extended_plan:?}"
        );
        seed_ties(4096, 8192);
        let larger = measure();
        assert_eq!(larger.0.as_deref(), Some("tie-008191"));
        assert_eq!(
            larger.1, extended.1,
            "the complete query's instruction/sort/fullscan counts must not grow with the tie group"
        );
        assert_eq!(
            prepare(read_tx(&db, &run()).unwrap()).unwrap().value,
            canonical(&db, &run())
        );
        eprintln!(
            "Run latest-Turn counts (VM/sort/fullscan): old32={:?}, old4096={:?}, extended4096={:?}, extended8192={:?}",
            old_small.1, old_dense.1, extended.1, larger.1
        );
    }

    #[test]
    fn closed_targets_do_no_sql_and_raw_facts_outlive_connection() {
        let db = fixture();
        let trace = Statements::new(&db);
        for command in [
            Command::RunGet(crate::protocol::RunTarget {
                run_id: String::new(),
            }),
            Command::TurnGet(crate::protocol::TurnTarget {
                turn_id: "x".repeat(4097),
            }),
            Command::ShellGet(crate::protocol::ExecutionTarget {
                execution_id: "x".into(),
            }),
        ] {
            assert!(read_tx(&db, &command).is_err());
        }
        assert_eq!(trace.count(), 0);
        drop(trace);
        let raw = read_tx(&db, &turn()).unwrap();
        drop(db);
        assert_eq!(prepare(raw).unwrap().value["turn_id"], "u");
    }
    #[test]
    fn limited_recovery_hint_follows_language_without_changing_saved_reason() {
        use crate::config::UiLanguage::{En, Ja};
        let db = fixture();
        let saved = json!({"message": "x".repeat(JSON_BYTES)}).to_string();
        db.execute("UPDATE turn SET recovery_reason_json=?1", [&saved])
            .unwrap();
        for command in [run(), turn()] {
            let ja = prepare_in(read_tx(&db, &command).unwrap(), Ja).unwrap();
            let en = prepare_in(read_tx(&db, &command).unwrap(), En).unwrap();
            assert!(
                ja.value["recovery_hint"]
                    .as_str()
                    .unwrap()
                    .contains("取得範囲外")
            );
            assert!(
                en.value["recovery_hint"]
                    .as_str()
                    .unwrap()
                    .contains("Unavailable")
            );
            assert_eq!(ja.value["context"], en.value["context"]);
            assert_eq!(ja.limited_fields, en.limited_fields);
        }
        let unchanged: String = db
            .query_row(
                "SELECT recovery_reason_json FROM turn WHERE id='u'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unchanged, saved);
        let original = Some("user source 日本語".to_owned());
        assert_eq!(reason_hint(original.clone(), false, En), original);
    }
    #[test]
    fn reason_is_budgeted_and_revisit_summary_is_read_from_receipts() {
        let db = fixture();
        let reason = json!({"message":"x".repeat(JSON_BYTES-100)}).to_string();
        db.execute("UPDATE turn SET recovery_reason_json=?1", [reason])
            .unwrap();
        let raw = read_tx(&db, &run()).unwrap();
        let RawBody::Run(body) = &raw.body else {
            panic!()
        };
        assert!(body.reason.as_ref().unwrap().len() <= JSON_BYTES);
        assert_eq!(
            body.revisit,
            Some(crate::protocol::RevisitSummary {
                pending: false,
                consumed_count: 3
            })
        );
        db.execute(
            "UPDATE turn SET recovery_reason_json=?1",
            [json!({"message":"x".repeat(JSON_BYTES)}).to_string()],
        )
        .unwrap();
        let value = prepare(read_tx(&db, &turn()).unwrap()).unwrap();
        assert_eq!(
            value.value["context"]["recovery_reason"]["observation"],
            "unavailable"
        );
        assert!(value.limited_fields.contains(&"recovery_hint".into()));
        let mut raw = read_tx(&db, &turn()).unwrap();
        // Current SQL rejects malformed JSON; independently exercise the pure
        // preparation boundary without weakening that database constraint.
        let RawBody::Turn(body) = &mut raw.body else {
            panic!()
        };
        body.reason = Some("broken".into());
        body.reason_limited = false;
        assert_eq!(prepare(raw).err().unwrap().code, ErrorCode::InvalidState);
    }
    #[test]
    fn oversized_scalar_is_explicit_unavailable_instead_of_invented_fact() {
        let db = fixture();
        db.execute("UPDATE run SET stop_reason=?1", ["x".repeat(513)])
            .unwrap();
        assert_eq!(
            read_tx(&db, &run()).err().unwrap().into_api_error().code,
            ErrorCode::InputLimit
        );
        db.execute("UPDATE turn SET ended_at=?1", ["x".repeat(513)])
            .unwrap();
        assert_eq!(
            read_tx(&db, &turn()).err().unwrap().into_api_error().code,
            ErrorCode::InputLimit
        );
    }
    #[tokio::test]
    async fn initial_requests_preserve_complete_canonical_totals_and_deleted_scope_marker() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config,"default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = crate::control::Control::new(&owner, root.path().into());
        owner.db.call(|db|{
            seed(db);
            db.execute_batch("UPDATE turn SET state='failed',ended_at='2026-01-01T00:00:02Z';UPDATE run SET state='completed',ended_at='2026-01-01T00:00:03Z';")?;
            Ok(())
        }).await.unwrap();
        let mut observed = Vec::new();
        for deleted in [false, true] {
            if deleted {
                owner.db.call(|db|{db.execute("UPDATE thread SET deleted_at='2026-01-01T00:00:04Z' WHERE id='foreign'",[])?;Ok(())}).await.unwrap();
            }
            for command in [run(), turn()] {
                let original = command.clone();
                let expected = owner
                    .db
                    .call(move |db| {
                        let mut value = canonical(db, &original);
                        let scope = match &original {
                            Command::RunGet(a) => crate::usage_totals::Scope::Run(a.run_id.clone()),
                            Command::TurnGet(a) => {
                                crate::usage_totals::Scope::Turn(a.turn_id.clone())
                            }
                            _ => unreachable!(),
                        };
                        value["usage_totals"] = crate::control::db::saved_usage_for_detail(
                            db,
                            &scope,
                            Default::default(),
                            None,
                        )?;
                        Ok(value)
                    })
                    .await
                    .unwrap();
                let actual = super::super::management_request(
                    &control,
                    command,
                    tokio_util::sync::CancellationToken::new(),
                )
                .await;
                observed.push((actual, expected));
            }
        }
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        for (actual, expected) in observed {
            let mut actual = actual.unwrap();
            assert_eq!(actual["view_observation"]["thread_id"], "foreign");
            assert!(
                actual["view_observation"]["observed_at"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
            actual.as_object_mut().unwrap().remove("view_observation");
            assert_eq!(actual, expected);
        }
    }
}
