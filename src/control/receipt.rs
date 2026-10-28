//! Receipt budget and retention on the existing serialized DB connection.
use crate::protocol::{ApiError, Command, ErrorCode, MAX_FRAME_BYTES, Request, Response};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub(super) const MAX_OUTCOME_BYTES: u64 = 8 * 1024 * 1024;
const TOTAL_BYTES: u64 = 256 * 1024 * 1024;
// Two bounded IPC stop requests can reserve their maximum outcome and metadata
// even when ordinary admission has consumed its entire budget.
const CONTROL_HEADROOM_BYTES: u64 = 2 * (MAX_OUTCOME_BYTES + MAX_FRAME_BYTES as u64 + 4096);
const CONTROL_TOTAL_BYTES: u64 = TOTAL_BYTES + CONTROL_HEADROOM_BYTES;

#[derive(Clone, Copy)]
pub(super) enum Admission {
    Normal,
    Stop,
}
impl Admission {
    pub(super) fn for_command(command: &Command) -> Self {
        match command {
            Command::RunStop(_) | Command::ShellStop(_) => Self::Stop,
            _ => Self::Normal,
        }
    }
    fn limit(self) -> u64 {
        match self {
            Self::Normal => TOTAL_BYTES,
            Self::Stop => CONTROL_TOTAL_BYTES,
        }
    }
}
const ROW_BYTES: u64 = 512;
const CLEANUP_BATCH: usize = 128;
const RESULT_RETENTION_SECONDS: u64 = 24 * 60 * 60;
// A single-byte JSON marker always shrinks a valid SavedOutcome object. Keep
// its identity/hash row so current-instance retransmission cannot run again.
pub(super) const EXPIRED_OUTCOME: &str = "0";
pub(super) fn expired_error() -> ApiError {
    let mut error = ApiError::new(
        ErrorCode::OperationExpired,
        "The saved operation result has expired; the original operation will not be executed again",
    );
    error.hint=Some("Inspect current state with shellboy operation get --operation ID and the target's get command. A new operation ID starts a new request; check side effects before doing so.".into());
    error
}
type Result<T> = std::result::Result<T, ApiError>;
fn sql(_: rusqlite::Error) -> ApiError {
    crate::storage::save_error("Cannot maintain operation receipts")
}
fn size(value: &str) -> u64 {
    value.len() as u64
}

pub(super) fn usage(db: &Connection, instance: &str) -> Result<u64> {
    // Actual bytes are derived transactionally by migration triggers; current
    // pending reservations use a partial index instead of scanning saved bodies.
    db.query_row("SELECT q.logical_bytes+(SELECT COALESCE(SUM(reserved_bytes),0) FROM operation_receipt INDEXED BY operation_receipt_pending_budget WHERE instance_id=?1 AND state='pending') FROM quota_counter q WHERE q.category='operation_receipt'",[instance],|r|r.get::<_,i64>(0).map(|n|n as u64)).map_err(sql)
}
/// Protected intent is never removed. Current identities keep an expired-result
/// marker; only old-instance identities may be deleted. All call sites own a
/// transaction, and one pass touches at most CLEANUP_BATCH rows.
pub(super) fn collect(db: &Connection, instance: &str, pressure: bool) -> Result<u64> {
    let rows=db.prepare("SELECT operation_id,(instance_id=?1 OR substr(operation_id,1,length(?1)+1)=?1||':'),COALESCE(octet_length(result_ref_json),0) FROM operation_receipt o
        WHERE state IN ('completed','failed')
        AND NOT EXISTS(SELECT 1 FROM receipt r WHERE r.client_operation_id=o.operation_id AND r.state IN ('accepted','held'))
        AND NOT EXISTS(SELECT 1 FROM attachment_delivery d WHERE d.operation_id=o.operation_id AND d.status IN ('accepted','held'))
        AND NOT EXISTS(SELECT 1 FROM app_meta m,json_each(CASE WHEN json_valid(m.value) THEN m.value ELSE '[]' END) x WHERE (m.key LIKE 'compact.pending.%' OR m.key LIKE 'compact.inflight.%') AND x.value=o.operation_id)
        AND (((instance_id=?1 OR substr(operation_id,1,length(?1)+1)=?1||':')
            AND result_ref_json IS NOT NULL AND result_ref_json<>?4 AND octet_length(result_ref_json)>1
            AND (?2 OR julianday(created_at)<=julianday('now',?5)))
          OR (instance_id<>?1 AND substr(operation_id,1,length(?1)+1)<>?1||':'
            AND (?2 OR julianday(created_at)<=julianday('now','-7 days'))))
        ORDER BY created_at,operation_id LIMIT ?3").map_err(sql)?.query_map(params![instance, pressure, CLEANUP_BATCH as i64, EXPIRED_OUTCOME, format!("-{RESULT_RETENTION_SECONDS} seconds")], |r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?,r.get::<_,i64>(2)?))).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    let mut collected = 0;
    // Preserve malformed durable envelopes. Parse one body at a time and cap
    // aggregate validation bytes as well as row count on this DB-worker pass.
    let mut examined_bytes = 0u64;
    for (id, current, body_bytes) in &rows {
        if *current {
            let bytes = u64::try_from(*body_bytes).unwrap_or(u64::MAX);
            if bytes > MAX_OUTCOME_BYTES || examined_bytes.saturating_add(bytes) > MAX_OUTCOME_BYTES
            {
                continue;
            }
            examined_bytes += bytes;
            let body: String = db
                .query_row(
                    "SELECT result_ref_json FROM operation_receipt WHERE operation_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .map_err(sql)?;
            if !super::db::valid_saved_outcome(&body) {
                continue;
            }
        }
        db.execute(
            "DELETE FROM app_meta WHERE key=?1",
            [crate::compaction::CompactionKeys::result(id)],
        )
        .map_err(sql)?;
        if *current {
            db.execute("UPDATE operation_receipt SET result_ref_json=?2,reserved_bytes=0 WHERE operation_id=?1", params![id,EXPIRED_OUTCOME]).map_err(sql)?;
        } else {
            db.execute("DELETE FROM operation_receipt WHERE operation_id=?1", [id])
                .map_err(sql)?;
        }
        collected += 1;
    }
    Ok(collected)
}

pub(super) fn reserve(
    db: &Connection,
    instance: &str,
    operation: &str,
    origin: &str,
    fingerprint: &str,
    now: &str,
    admission: Admission,
) -> Result<()> {
    reserve_with_limit(
        db,
        instance,
        operation,
        origin,
        fingerprint,
        now,
        admission.limit(),
    )
}
fn reserve_with_limit(
    db: &Connection,
    instance: &str,
    operation: &str,
    origin: &str,
    fingerprint: &str,
    now: &str,
    total: u64,
) -> Result<()> {
    let required = ROW_BYTES
        + size(operation)
        + size(instance)
        + size(origin)
        + size(fingerprint)
        + size(now)
        + MAX_OUTCOME_BYTES;
    if usage(db, instance)?.saturating_add(required) > total {
        // Each batch stays bounded; refusal can be retried while maintenance
        // continues rather than doing unbounded cleanup inside admission.
        collect(db, instance, true)?;
    }
    if usage(db, instance)?.saturating_add(required) > total {
        let mut error = ApiError::new(
            ErrorCode::StorageLimit,
            "Operation receipt budget is full; the command was not accepted",
        );
        error.hint=Some("Current terminal results are reclaimed under pressure before this refusal. Inspect shellboy operation get --operation ID and pending/protected work with shellboy status. Wait for pending work to finish; if identity metadata or protected receipts still fill the budget, finish current work and restart the owner explicitly. Same-ID replay may return operation_expired and never reexecutes the command.".into());
        return Err(error);
    }
    db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes)VALUES(?1,?2,?3,?4,'pending',?5,?6)",params![operation,instance,origin,fingerprint,now,MAX_OUTCOME_BYTES as i64]).map_err(sql)?;
    Ok(())
}
/// Includes recovery hints and the exact envelope, rather than assuming that a
/// result as large as the transport frame still fits after serialization.
pub(super) fn encode(
    operation: &str,
    outcome: &std::result::Result<Value, ApiError>,
) -> Result<String> {
    let saved = match outcome {
        Ok(result) => json!({"result":result}),
        Err(error) => json!({"error":error}),
    };
    let body = serde_json::to_string(&saved).expect("JSON outcome");
    let response = match outcome {
        Ok(value) => Response::success(operation, value.clone()),
        Err(error) => Response::failure(operation, error.clone()),
    };
    if size(&body) > MAX_OUTCOME_BYTES
        || serde_json::to_vec(&response).expect("JSON response").len() > MAX_FRAME_BYTES
    {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Operation result exceeds the receipt or response frame bound; use a smaller page or narrower query",
        ));
    }
    Ok(body)
}
pub(crate) fn summary(db: &Connection, instance: &str) -> Result<Value> {
    let used = usage(db, instance)?;
    let protected_old:i64=db.query_row("SELECT q.logical_bytes-(SELECT COALESCE(SUM(512+octet_length(operation_id)+octet_length(instance_id)+octet_length(origin_json)+octet_length(request_hash)+octet_length(created_at)+COALESCE(octet_length(result_ref_json),0)),0) FROM operation_receipt INDEXED BY operation_receipt_pending_budget WHERE instance_id=?1 AND state='pending') FROM quota_counter q WHERE category='operation_receipt.pending_actual'",[instance],|r|r.get(0)).map_err(sql)?;
    Ok(
        json!({"accounted_bytes":used,"limit_bytes":TOTAL_BYTES,"max_outcome_bytes":MAX_OUTCOME_BYTES,"result_retention_seconds":RESULT_RETENTION_SECONDS,"result_retention":"24h_or_pressure","expired_replay":"operation_expired","control_headroom_bytes":CONTROL_HEADROOM_BYTES,"control_limit_bytes":CONTROL_TOTAL_BYTES,"control_available_bytes":CONTROL_TOTAL_BYTES.saturating_sub(used),"protected_old_pending_bytes":protected_old,"at_limit":used>=TOTAL_BYTES,"available_bytes":TOTAL_BYTES.saturating_sub(used),"full_result_admission_blocked":used.saturating_add(MAX_OUTCOME_BYTES)>TOTAL_BYTES,"recovery":"same_id_replay_or_expired_status_internal_gc_explicit_restart"}),
    )
}

pub(super) fn under_pressure(db: &Connection, instance: &str) -> Result<bool> {
    Ok(usage(db, instance)?.saturating_add(MAX_OUTCOME_BYTES) > TOTAL_BYTES)
}
/// Runtime mutations have scalar acknowledgments; collection has at most 128
/// IDs and 128 bounded failure details. Reject an envelope that cannot carry
/// that certified 512KiB result before starting external effects.
pub(super) fn runtime_preflight(request: &Request) -> Result<()> {
    if matches!(
        request.command,
        Command::ShellWrite(_)
            | Command::ShellEof(_)
            | Command::ShellStop(_)
            | Command::IndexBackfill(_)
            | Command::IndexRetry(_)
            | Command::IndexRebuild(_)
            | Command::StorageCollect(_)
    ) {
        let base = serde_json::to_vec(&Response::success(&request.operation_id, Value::Null))
            .expect("JSON response")
            .len();
        if base + 512 * 1024 > MAX_FRAME_BYTES {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Operation ID leaves insufficient room for the bounded runtime acknowledgment",
            ));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db
    }
    fn scanned_usage(db: &Connection, instance: &str) -> u64 {
        db.query_row("SELECT COALESCE(SUM(length(CAST(operation_id AS BLOB))+length(CAST(instance_id AS BLOB))+length(CAST(origin_json AS BLOB))+length(CAST(request_hash AS BLOB))+length(CAST(created_at AS BLOB))+COALESCE(length(CAST(result_ref_json AS BLOB)),0)+512+CASE WHEN instance_id=?1 AND state='pending' THEN reserved_bytes ELSE 0 END),0) FROM operation_receipt",[instance],|r|r.get::<_,i64>(0)).unwrap() as u64
    }
    #[test]
    fn real_budget_admits_31_full_reservations_and_completion_reopens_capacity() {
        let db = fixture();
        for n in 0..31 {
            reserve(
                &db,
                "current",
                &format!("current:{n}"),
                "null",
                "hash",
                "now",
                Admission::Normal,
            )
            .unwrap();
        }
        assert_eq!(
            reserve(
                &db,
                "current",
                "current:32",
                "null",
                "hash",
                "now",
                Admission::Normal
            )
            .unwrap_err()
            .code,
            ErrorCode::StorageLimit
        );
        assert_eq!(collect(&db, "current", true).unwrap(), 0);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            31
        );
        db.execute("UPDATE operation_receipt SET state='completed',result_ref_json='{}',reserved_bytes=0 WHERE operation_id='current:0'",[]).unwrap();
        reserve(
            &db,
            "current",
            "current:32",
            "null",
            "hash",
            "now",
            Admission::Normal,
        )
        .unwrap();
        assert_eq!(
            collect(&db, "current", true).unwrap(),
            0,
            "malformed fixture envelopes are retained, not hidden by expiry"
        );
    }
    #[test]
    fn current_result_pressure_reclaims_body_but_preserves_identity_and_counter() {
        let db = fixture();
        let budget = MAX_OUTCOME_BYTES + 2048;
        reserve_with_limit(
            &db,
            "current",
            "current:first",
            "null",
            "hash",
            "now",
            budget,
        )
        .unwrap();
        let outcome = encode("current:first", &Ok(json!({"text":"x".repeat(1500)}))).unwrap();
        db.execute(
            "UPDATE operation_receipt SET state='completed',result_ref_json=?1,reserved_bytes=0",
            [&outcome],
        )
        .unwrap();
        let before = usage(&db, "current").unwrap();
        reserve_with_limit(
            &db,
            "current",
            "current:next",
            "null",
            "hash",
            "now",
            budget,
        )
        .unwrap();
        assert_eq!(
            db.query_row(
                "SELECT result_ref_json FROM operation_receipt WHERE operation_id='current:first'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            EXPIRED_OUTCOME
        );
        assert_eq!(collect(&db, "current", true).unwrap(), 0);
        assert_eq!(
            usage(&db, "current").unwrap(),
            scanned_usage(&db, "current")
        );
        db.execute(
            "DELETE FROM operation_receipt WHERE operation_id='current:next'",
            [],
        )
        .unwrap();
        assert_eq!(
            usage(&db, "current").unwrap(),
            before - outcome.len() as u64 + 1
        );
        assert_eq!(
            super::super::db::operation_details(&db, "current:first", "current").unwrap()["result_expired"],
            true
        );
        assert_eq!(
            super::super::db::operation_details(&db, "current:first", "current").unwrap()["state"],
            "completed"
        );
        assert_eq!(collect(&db, "restarted", true).unwrap(), 1);
        assert_eq!(usage(&db, "restarted").unwrap(), 0);
    }
    #[test]
    fn current_ttl_and_pressure_protect_pending_deliveries_and_compaction() {
        let db = fixture();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('fixture-thread','/fixture','/fixture/AGENTS.md','fixture-profile','fixture-model','now'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('fixture-node','user','now','{}');").unwrap();
        for (id, state) in [
            ("expired", "completed"),
            ("failed", "failed"),
            ("pending", "pending"),
            ("message", "completed"),
            ("attachment", "completed"),
            ("compact-pending", "completed"),
            ("compact-inflight", "completed"),
        ] {
            db.execute(r#"INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES(?1,'current','null','hash',?2,'2000-01-01','{"result":{}}',0)"#,params![format!("current:{id}"),state]).unwrap();
            db.execute(
                r#"INSERT INTO app_meta VALUES(?1,'{"status":"completed"}')"#,
                [format!("compact.result.current:{id}")],
            )
            .unwrap();
        }
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at) VALUES('message-receipt','current:message','{}','fixture-thread','fixture-node','held','now')", [])
            .unwrap();
        db.execute(
            "INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at) VALUES('fixture-node','fixture-thread','accepted','current:attachment','{}','now')",
            [],
        )
        .unwrap();
        db.execute(r#"INSERT INTO app_meta VALUES('compact.pending.fixture','["current:compact-pending"]')"#,[]).unwrap();
        db.execute(r#"INSERT INTO app_meta VALUES('compact.inflight.fixture','["current:compact-inflight"]')"#,[]).unwrap();
        db.execute(r#"INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('current:fresh','current','null','hash','completed',datetime('now','-23 hours'),'{"result":{}}',0)"#,[]).unwrap();
        assert_eq!(collect(&db, "current", false).unwrap(), 2);
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM operation_receipt WHERE result_ref_json='0'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key IN ('compact.result.current:expired','compact.result.current:failed')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(collect(&db, "current", true).unwrap(), 1);
        for id in [
            "pending",
            "message",
            "attachment",
            "compact-pending",
            "compact-inflight",
        ] {
            assert_ne!(
                db.query_row(
                    "SELECT result_ref_json FROM operation_receipt WHERE operation_id=?1",
                    [format!("current:{id}")],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                EXPIRED_OUTCOME
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key=?1",
                    [format!("compact.result.current:{id}")],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
        assert_eq!(
            usage(&db, "current").unwrap(),
            scanned_usage(&db, "current")
        );
    }
    #[test]
    fn expiry_is_batched_and_never_hides_corrupt_outcomes_or_deletes_current_ids() {
        let db = fixture();
        for n in 0..130 {
            db.execute(r#"INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES(?1,'current','null','hash','completed','2000-01-01','{"result":{}}',0)"#,[format!("current:{n:03}")]).unwrap();
        }
        assert_eq!(collect(&db, "current", true).unwrap(), 128);
        assert_eq!(collect(&db, "current", true).unwrap(), 2);
        assert_eq!(collect(&db, "current", true).unwrap(), 0);
        assert_eq!(
            db.query_row("SELECT count(*) FROM operation_receipt", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            130
        );
        // Simulate existing corruption that current writer constraints reject.
        db.execute_batch("PRAGMA ignore_check_constraints=ON;")
            .unwrap();
        for (id, body) in [
            ("bad-json", "broken"),
            ("bad-envelope", "{}"),
            (
                "bad-error",
                r#"{"error":{"code":"unknown","message":"bad"}}"#,
            ),
        ] {
            db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES(?1,'current','null','hash','failed','2000-01-01',?2,0)",params![format!("current:{id}"),body]).unwrap();
        }
        db.execute_batch("PRAGMA ignore_check_constraints=OFF;")
            .unwrap();
        assert_eq!(collect(&db, "current", true).unwrap(), 0);
        assert_eq!(
            usage(&db, "current").unwrap(),
            scanned_usage(&db, "current")
        );
    }
    #[test]
    fn expiry_limits_examined_bytes_and_skips_oversized_legacy_rows() {
        let db = fixture();
        let large = encode(
            "current:a",
            &Ok(json!({"text":"x".repeat(MAX_OUTCOME_BYTES as usize/2)})),
        )
        .unwrap();
        for id in ["current:a", "current:b"] {
            db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES(?1,'current','null','hash','completed','2000-01-01',?2,0)",params![id,large]).unwrap();
        }
        db.execute(r#"INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('current:c','current','null','hash','completed','2000-01-01','{"result":{}}',0)"#,[]).unwrap();
        assert_eq!(
            collect(&db, "current", true).unwrap(),
            2,
            "one large outcome plus a small outcome fit; two large do not"
        );
        assert!(
            !db.query_row(
                "SELECT result_ref_json='0' FROM operation_receipt WHERE operation_id='current:b'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
        );
        assert_eq!(collect(&db, "current", true).unwrap(), 1);
        // An out-of-contract oversized legacy envelope is never materialized or
        // overwritten, and does not prevent a subsequent small row's progress.
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('current:oversized','current','null','hash','completed','2000-01-01',?1,0)",[serde_json::to_string(&"x".repeat(MAX_OUTCOME_BYTES as usize-1)).unwrap()]).unwrap();
        db.execute(r#"INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('current:z','current','null','hash','completed','2000-01-01','{"result":{}}',0)"#,[]).unwrap();
        assert_eq!(collect(&db, "current", true).unwrap(), 1);
        assert_eq!(db.query_row("SELECT octet_length(result_ref_json) FROM operation_receipt WHERE operation_id='current:oversized'",[],|r|r.get::<_,i64>(0)).unwrap(),MAX_OUTCOME_BYTES as i64+1);
        assert_eq!(collect(&db, "current", true).unwrap(), 0);
        assert_eq!(
            usage(&db, "current").unwrap(),
            scanned_usage(&db, "current")
        );
    }
    #[test]
    fn failed_current_expiry_rolls_back_companion_and_quota_updates() {
        let mut db = fixture();
        db.execute_batch(r#"INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('current:done','current','null','hash','completed','2000-01-01','{"result":{}}',0);
            INSERT INTO app_meta VALUES('compact.result.current:done','{}');
            CREATE TRIGGER fail_expiry BEFORE UPDATE ON operation_receipt BEGIN SELECT RAISE(ABORT,'fixture');END;"#).unwrap();
        let before = usage(&db, "current").unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(collect(&tx, "current", true).is_err());
        }
        assert_eq!(usage(&db, "current").unwrap(), before);
        assert_eq!(
            usage(&db, "current").unwrap(),
            scanned_usage(&db, "current")
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE key='compact.result.current:done'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    #[test]
    fn reservations_are_serialized_and_current_and_unknown_intents_are_protected() {
        let mut db = fixture();
        let budget = MAX_OUTCOME_BYTES + 1024;
        let tx = db.transaction().unwrap();
        reserve_with_limit(
            &tx,
            "current",
            "current:first",
            "null",
            "hash",
            "now",
            budget,
        )
        .unwrap();
        assert_eq!(
            reserve_with_limit(
                &tx,
                "current",
                "current:second",
                "null",
                "hash",
                "now",
                budget
            )
            .unwrap_err()
            .code,
            ErrorCode::StorageLimit
        );
        tx.execute(
            "UPDATE operation_receipt SET state='completed',result_ref_json='{}',reserved_bytes=0",
            [],
        )
        .unwrap();
        reserve_with_limit(
            &tx,
            "current",
            "current:second",
            "null",
            "hash",
            "now",
            budget + 1024,
        )
        .unwrap();
        tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('old:pending','old','null','hash','pending','2000-01-01',NULL,8388608)",[]).unwrap();
        assert_eq!(collect(&tx, "current", true).unwrap(), 0);
        tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('old:done','old','null','hash','completed','2000-01-01','{}',0)",[]).unwrap();
        assert_eq!(collect(&tx, "current", false).unwrap(), 1);
        assert_eq!(
            tx.query_row("SELECT count(*) FROM operation_receipt", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        tx.commit().unwrap();
    }
    #[test]
    fn response_bound_includes_the_envelope_and_escaped_operation_id() {
        let outcome = Ok(json!({"body":"x".repeat(MAX_FRAME_BYTES-100)}));
        assert_eq!(
            encode(&"\\".repeat(100), &outcome).unwrap_err().code,
            ErrorCode::InputLimit
        );
        assert!(encode("current:small", &Ok(json!({"ok":true}))).is_ok());
    }
    #[test]
    fn derived_counters_follow_direct_sql_unicode_updates_deletes_and_rollback() {
        let mut db = fixture();
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('current:日本語','current','{\"execution_id\":\"猫\"}','hash','pending','now',NULL,4096)",[]).unwrap();
        assert_eq!(
            usage(&db, "current").unwrap(),
            scanned_usage(&db, "current")
        );
        assert_eq!(
            summary(&db, "next").unwrap()["protected_old_pending_bytes"],
            scanned_usage(&db, "next")
        );
        let before = usage(&db, "current").unwrap();
        {
            let tx = db.transaction().unwrap();
            tx.execute(
                "UPDATE operation_receipt SET result_ref_json=?1,reserved_bytes=9999",
                [json!({"result":"🐈é".repeat(50)}).to_string()],
            )
            .unwrap();
            assert_eq!(
                usage(&tx, "current").unwrap(),
                scanned_usage(&tx, "current")
            );
        }
        assert_eq!(usage(&db, "current").unwrap(), before);
        db.execute("UPDATE operation_receipt SET instance_id='next',state='completed',result_ref_json=?1,reserved_bytes=0",[json!({"result":"🐈é"}).to_string()]).unwrap();
        assert_eq!(usage(&db, "next").unwrap(), scanned_usage(&db, "next"));
        assert_eq!(
            summary(&db, "current").unwrap()["protected_old_pending_bytes"],
            0
        );
        db.execute("DELETE FROM operation_receipt", []).unwrap();
        assert_eq!(usage(&db, "current").unwrap(), 0);
        assert_eq!(
            summary(&db, "current").unwrap()["protected_old_pending_bytes"],
            0
        );
        let plan:String=db.query_row("EXPLAIN QUERY PLAN SELECT SUM(reserved_bytes) FROM operation_receipt INDEXED BY operation_receipt_pending_budget WHERE instance_id='current' AND state='pending'",[],|r|r.get(3)).unwrap();
        assert!(plan.contains("operation_receipt_pending_budget"));
    }

    #[test]
    fn failed_collection_rolls_back_counter_and_companion_changes() {
        let mut db = fixture();
        db.execute_batch("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes) VALUES('old:done','old','null','hash','completed','2000-01-01','{}',0);INSERT INTO app_meta VALUES('compact.result.old:done','{}');CREATE TRIGGER fail_collection BEFORE DELETE ON operation_receipt BEGIN SELECT RAISE(ABORT,'fixture');END;").unwrap();
        let before = usage(&db, "current").unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(
                collect(&tx, "current", true)
                    .unwrap_err()
                    .is_storage_failure()
            );
        }
        assert_eq!(usage(&db, "current").unwrap(), before);
        assert_eq!(
            usage(&db, "current").unwrap(),
            scanned_usage(&db, "current")
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE key='compact.result.old:done'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    #[ignore = "manual near-quota DB timing; synthetic hundreds of MiB"]
    fn measure_receipt_accounting_near_quota() {
        use std::time::Instant;
        for (label, count, chars) in [
            ("large_results", 1024, 248 * 1024),
            ("many_small_results", 200_000, 700),
            ("pending_reservations", 31, 0),
        ] {
            let mut db = fixture();
            let body = json!({"result":"x".repeat(chars)}).to_string();
            {
                let tx = db.transaction().unwrap();
                let mut insert=tx.prepare("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json,reserved_bytes)VALUES(?1,'current','null','hash',?3,'2026-10-25',?2,?4)").unwrap();
                for n in 0..count {
                    let pending = label == "pending_reservations";
                    insert
                        .execute(params![
                            format!("current:{n}"),
                            if pending { None } else { Some(&body) },
                            if pending { "pending" } else { "completed" },
                            if pending { MAX_OUTCOME_BYTES as i64 } else { 0 }
                        ])
                        .unwrap();
                }
                drop(insert);
                tx.commit().unwrap();
            }
            let expected = scanned_usage(&db, "current");
            assert_eq!(usage(&db, "current").unwrap(), expected);
            let mut scan = Vec::new();
            let mut cached = Vec::new();
            for _ in 0..7 {
                let at = Instant::now();
                assert_eq!(scanned_usage(&db, "current"), expected);
                scan.push(at.elapsed().as_secs_f64() * 1000.0);
                let at = Instant::now();
                assert_eq!(usage(&db, "current").unwrap(), expected);
                cached.push(at.elapsed().as_secs_f64() * 1000.0);
            }
            scan.sort_by(f64::total_cmp);
            cached.sort_by(f64::total_cmp);
            eprintln!(
                "{label}: rows={count}, accounted_bytes={expected}, scan_ms p50={:.3} max={:.3}; derived_ms p50={:.3} max={:.3}",
                scan[3], scan[6], cached[3], cached[6]
            );
        }
    }
}
