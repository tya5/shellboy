//! Exact saved operation observation; no receipt replay or prefix authority.
//! JSON is length-gated before copy, and decoded after the RO transaction ends.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
const JSON_BYTES: usize = 64 * 1024;
const OP_SQL:&str="SELECT
 CASE WHEN octet_length(instance_id)<=512 THEN instance_id END,
 CASE WHEN octet_length(created_at)<=512 THEN created_at END,
 CASE WHEN octet_length(state)<=512 THEN state END,
 CASE WHEN octet_length(result_ref_json)<=65536 THEN result_ref_json END,
 octet_length(result_ref_json),
 CASE WHEN octet_length(result_ref_json)<=65536 THEN CASE WHEN json_valid(result_ref_json) THEN
   CASE WHEN json_type(result_ref_json,'$.result.thread_id')='text' AND octet_length(json_extract(result_ref_json,'$.result.thread_id'))<=512 THEN json_extract(result_ref_json,'$.result.thread_id') END END END,
 CASE WHEN octet_length(result_ref_json)<=65536 THEN CASE WHEN json_valid(result_ref_json) THEN
   CASE WHEN json_type(result_ref_json,'$.result.message_id')='text' AND octet_length(json_extract(result_ref_json,'$.result.message_id'))<=512 THEN json_extract(result_ref_json,'$.result.message_id') END END END,
 CASE WHEN octet_length(result_ref_json)<=65536 THEN CASE WHEN json_valid(result_ref_json) THEN
   json_extract(result_ref_json,'$.result.status')='queued' AND json_extract(result_ref_json,'$.result.operation_id')=operation_id ELSE 0 END ELSE 0 END,
 CASE WHEN octet_length(result_ref_json)<=65536 THEN CASE WHEN json_valid(result_ref_json) THEN
 COALESCE(json_type(result_ref_json,'$.result.thread_id')='text' AND octet_length(json_extract(result_ref_json,'$.result.thread_id'))>512,0) OR COALESCE(json_type(result_ref_json,'$.result.message_id')='text' AND octet_length(json_extract(result_ref_json,'$.result.message_id'))>512,0) ELSE 0 END ELSE 0 END
 FROM operation_receipt WHERE operation_id=?1";
#[derive(Debug)]
pub(super) struct RawOperation {
    id: String,
    instance: String,
    current_instance: String,
    created: String,
    state: String,
    body: Option<String>,
    body_bytes: Option<u64>,
    thread: Option<String>,
    downstream: Downstream,
    identity_limited: bool,
    observed_at: String,
    #[cfg(test)]
    steps: u64,
    #[cfg(test)]
    statements: usize,
}
#[derive(Debug)]
enum Downstream {
    None,
    Message {
        state: Option<String>,
        reason: Option<String>,
        limited: bool,
    },
    Compact {
        saved: Witness,
        pending: Witness,
        inflight: Witness,
    },
    Unavailable,
}
#[derive(Debug, Default)]
struct Witness {
    present: bool,
    body: Option<String>,
    bytes: usize,
}
fn invalid(message: &str) -> StorageError {
    ApiError::new(ErrorCode::InputLimit, message).into()
}
fn scalar(value: Option<String>) -> Result<String, StorageError> {
    value.ok_or_else(|| {
        invalid(
            "Saved operation scalar exceeds its512-byte display limit; use explicit operation get",
        )
    })
}
#[derive(Default)]
struct Trace {
    #[cfg(test)]
    steps: u64,
    #[cfg(test)]
    statements: usize,
}
impl Trace {
    fn record(&mut self, _statement: &rusqlite::Statement<'_>) {
        #[cfg(test)]
        {
            self.steps += _statement.get_status(rusqlite::StatementStatus::VmStep) as u64;
            self.statements += 1;
        }
    }
}
fn metadata(
    db: &Connection,
    key: &str,
    budget: &mut usize,
    trace: &mut Trace,
) -> Result<Witness, StorageError> {
    let mut stmt=db.prepare("SELECT CASE WHEN octet_length(value)<=?2 THEN value END,octet_length(value) FROM app_meta WHERE key=?1")?;
    let row = stmt
        .query_row(params![key, *budget as i64], |r| {
            let bytes: i64 = r.get(1)?;
            let bytes = usize::try_from(bytes)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, bytes))?;
            Ok((r.get::<_, Option<String>>(0)?, bytes))
        })
        .optional()?;
    trace.record(&stmt);
    Ok(match row {
        None => Witness::default(),
        Some((body, bytes)) => {
            *budget = budget.saturating_sub(bytes);
            Witness {
                present: true,
                body,
                bytes,
            }
        }
    })
}
pub(super) fn read_tx(
    db: &Connection,
    id: &str,
    current_instance: &str,
) -> Result<RawOperation, StorageError> {
    if id.is_empty() || id.len() > 4096 {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Operation inspection requires an exact1..4096-byte ID",
        )
        .into());
    }
    let mut stmt = db.prepare(OP_SQL)?;
    let row = stmt
        .query_row([id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?
                    .map(|bytes| {
                        u64::try_from(bytes)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, bytes))
                    })
                    .transpose()?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<bool>>(7)?,
                r.get::<_, bool>(8)?,
            ))
        })
        .optional()?;
    let mut trace = Trace::default();
    trace.record(&stmt);
    let Some((
        instance,
        created,
        state,
        body,
        body_bytes,
        mut thread,
        message,
        queued,
        identity_limited,
    )) = row
    else {
        return Err(
            ApiError::new(ErrorCode::OperationNotFound, "operation receipt not found").into(),
        );
    };
    let (instance, created, state) = (scalar(instance)?, scalar(created)?, scalar(state)?);
    // This unique-index join resolves accepted-but-not-finalized message sends.
    // Neither an operation prefix nor the viewer supplies target authority.
    if thread.is_none() {
        let mut stmt=db.prepare("SELECT CASE WHEN octet_length(target_thread_id)<=512 THEN target_thread_id END FROM receipt WHERE client_operation_id=?1")?;
        thread = stmt
            .query_row([id], |r| r.get::<_, Option<String>>(0))
            .optional()?
            .flatten();
        trace.record(&stmt);
    }
    let mut budget =
        JSON_BYTES.saturating_sub(body_bytes.unwrap_or(0).min(JSON_BYTES as u64) as usize);
    let downstream = if queued == Some(true) {
        let saved = metadata(
            db,
            &crate::compaction::CompactionKeys::result(id),
            &mut budget,
            &mut trace,
        )?;
        let (pending, inflight) = if !saved.present {
            if let Some(thread) = &thread {
                (
                    metadata(
                        db,
                        &crate::compaction::CompactionKeys::new(thread).pending(),
                        &mut budget,
                        &mut trace,
                    )?,
                    metadata(
                        db,
                        &crate::compaction::CompactionKeys::new(thread).inflight(),
                        &mut budget,
                        &mut trace,
                    )?,
                )
            } else {
                (Witness::default(), Witness::default())
            }
        } else {
            (Witness::default(), Witness::default())
        };
        if !saved.present && thread.is_none() {
            Downstream::None
        } else {
            Downstream::Compact {
                saved,
                pending,
                inflight,
            }
        }
    } else if let Some(message) = message {
        let mut stmt=db.prepare("SELECT CASE WHEN octet_length(state)<=512 THEN state END,CASE WHEN octet_length(hold_reason)<=512 THEN hold_reason END,COALESCE(octet_length(state)>512,0) OR COALESCE(octet_length(hold_reason)>512,0),CASE WHEN octet_length(target_thread_id)<=512 THEN target_thread_id END FROM receipt WHERE message_node_id=?1 LIMIT 1")?;
        let row = stmt
            .query_row([message], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .optional()?;
        trace.record(&stmt);
        if let Some((state, reason, limited, actual_thread)) = row {
            if thread.is_none() {
                thread = actual_thread;
            }
            Downstream::Message {
                state,
                reason,
                limited,
            }
        } else {
            Downstream::None
        }
    } else if body_bytes.is_some_and(|bytes| bytes > JSON_BYTES as u64) {
        Downstream::Unavailable
    } else {
        Downstream::None
    };
    Ok(RawOperation {
        id: id.into(),
        instance,
        current_instance: current_instance.into(),
        created,
        state,
        body,
        body_bytes,
        thread,
        downstream,
        identity_limited,
        observed_at: chrono::Utc::now().to_rfc3339(),
        #[cfg(test)]
        steps: trace.steps,
        #[cfg(test)]
        statements: trace.statements,
    })
}
pub(super) fn prepare(
    raw: RawOperation,
) -> Result<super::inspection_saved::PreparedSaved, ApiError> {
    let (outcome, expired) =
        crate::control::receipt::decode_operation_outcome(raw.body.as_deref())?;
    let mut value = crate::control::receipt::operation_envelope(
        &raw.id,
        &raw.instance,
        &raw.created,
        &raw.state,
        &outcome,
        expired,
        &raw.current_instance,
    );
    let mut limited = Vec::new();
    if raw
        .body_bytes
        .is_some_and(|bytes| bytes > JSON_BYTES as u64)
    {
        value["outcome"] = json!({"observation":"unavailable","reason":"saved outcome exceeds64KiB display-copy limit","source_bytes":raw.body_bytes});
        value["targets"] = Value::Null;
        limited.push("outcome".into());
        limited.push("targets".into());
    }
    match raw.downstream {
        Downstream::None => {}
        Downstream::Unavailable => {
            value["downstream"] = json!({"available":false,"current":false,"reason":"Saved outcome identities were not copied within the display limit"});
            limited.push("downstream".into());
        }
        Downstream::Message {
            state,
            reason,
            limited: partial,
        } => {
            value["downstream"] = json!({"kind":"message","state":state});
            if let Some(reason) = reason {
                value["downstream"]["hold_reason"] = json!(reason);
            }
            if partial {
                value["downstream"]["available"] = json!(false);
                value["downstream"]["current"] = json!(false);
                limited.push("downstream".into());
            }
        }
        Downstream::Compact {
            saved,
            pending,
            inflight,
        } => {
            if saved.present {
                if let Some(body) = saved.body {
                    value["downstream"] = serde_json::from_str(&body).map_err(|_| {
                        ApiError::new(ErrorCode::InvalidState, "compaction outcome is malformed")
                    })?;
                } else {
                    value["downstream"] = json!({"kind":"compact","available":false,"current":false,"reason":"Saved compact result exceeds remaining64KiB witness budget","source_bytes":saved.bytes});
                    limited.push("downstream".into());
                }
            } else {
                let mut state = "unknown";
                let mut partial = false;
                for (witness, candidate) in [(pending, "queued"), (inflight, "running")] {
                    if let Some(body) = witness.body {
                        match serde_json::from_str::<Vec<String>>(&body) {
                            Ok(ids) => {
                                if ids.iter().any(|id| id == &raw.id) {
                                    state = candidate;
                                }
                            }
                            Err(_) => partial = true,
                        }
                    } else if witness.present {
                        partial = true;
                    }
                }
                value["downstream"] = json!({"kind":"compact","state":state});
                if partial {
                    value["downstream"]["available"] = json!(false);
                    value["downstream"]["current"] = json!(false);
                    limited.push("downstream".into());
                }
            }
        }
    }
    if raw.identity_limited {
        value["downstream"] = json!({"available":false,"current":false,"reason":"Saved downstream identity exceeds512-byte scalar inspection limit"});
        if !limited.iter().any(|field| field == "downstream") {
            limited.push("downstream".into());
        }
    }
    Ok(super::inspection_saved::PreparedSaved {
        value,
        thread: raw.thread,
        limited_fields: limited,
        observed_at: raw.observed_at,
    })
}

pub(super) async fn request(
    control: &std::sync::Arc<crate::control::Control>,
    id: String,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Value, ApiError> {
    let instance = control.store.instance_id.clone();
    let raw = super::snapshot::read(
        control.store.database.path().to_path_buf(),
        stop,
        move |db| read_tx(db, &id, &instance),
    )
    .await?;
    let mut prepared = super::presentation_work(move || prepare(raw)).await??;
    prepared.value["view_observation"] =
        json!({"thread_id":prepared.thread,"observed_at":prepared.observed_at});
    if !prepared.limited_fields.is_empty() {
        prepared.value["view_limits"] = json!({"limited_fields":prepared.limited_fields});
    }
    Ok(prepared.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('actual','','','fixture','model','now'),('viewer','','','fixture','model','now'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('message','user','now','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,hold_reason)VALUES('receipt','saved:message','{}','actual','message','held','now','test_hold');").unwrap();
        db
    }
    fn insert(db: &Connection, id: &str, state: &str, body: Option<&str>) {
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES(?1,'old-instance','null','hash',?2,?3,'now')",params![id,state,body]).unwrap();
    }
    fn parity(db: &Connection, id: &str) {
        let exact = crate::control::db::operation_details(db, id, "current").unwrap();
        let raw = read_tx(db, id, "current").unwrap();
        let prepared = prepare(raw).unwrap();
        assert_eq!(prepared.value, exact);
        assert!(prepared.limited_fields.is_empty());
    }
    #[test]
    fn complete_small_results_match_canonical_null_expired_old_instance_and_message_lifecycle() {
        let db = fixture();
        for (id, state, body) in [
            ("none", "pending", None),
            ("expired", "completed", Some("0")),
            (
                "old",
                "completed",
                Some("{\"result\":{\"run_id\":\"saved-run\",\"thread_id\":\"actual\"}}"),
            ),
        ] {
            insert(&db, id, state, body);
            parity(&db, id);
        }
        let expired = prepare(read_tx(&db, "expired", "current").unwrap()).unwrap();
        assert_eq!(expired.value["current_instance"], false);
        assert_eq!(expired.value["result_expired"], true);
        assert_eq!(expired.value["replay_policy"], "operation_expired");
        insert(&db, "saved:message", "pending", None);
        assert_eq!(
            read_tx(&db, "saved:message", "current")
                .unwrap()
                .thread
                .as_deref(),
            Some("actual")
        );
        parity(&db, "saved:message");
        db.execute("UPDATE operation_receipt SET state='completed',result_ref_json=?1 WHERE operation_id='saved:message'",["{\"result\":{\"message_id\":\"message\"}}"] ).unwrap();
        let first = read_tx(&db, "saved:message", "current").unwrap();
        assert_eq!(first.thread.as_deref(), Some("actual"));
        parity(&db, "saved:message");
        db.execute(
            "UPDATE receipt SET state='cancelled',hold_reason=NULL WHERE id='receipt'",
            [],
        )
        .unwrap();
        parity(&db, "saved:message");
        assert_eq!(
            prepare(read_tx(&db, "saved:message", "current").unwrap())
                .unwrap()
                .value["downstream"]["state"],
            "cancelled"
        );
    }
    #[test]
    fn first_compact_snapshot_resolves_queued_running_and_saved_without_followup() {
        let db = fixture();
        let id = "compact";
        insert(
            &db,
            id,
            "completed",
            Some(
                "{\"result\":{\"status\":\"queued\",\"operation_id\":\"compact\",\"thread_id\":\"actual\"}}",
            ),
        );
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('compact.pending.actual','[\"compact\"]')",
            [],
        )
        .unwrap();
        parity(&db, id);
        db.execute_batch("DELETE FROM app_meta;INSERT INTO app_meta(key,value)VALUES('compact.inflight.actual','[\"compact\"]')").unwrap();
        parity(&db, id);
        db.execute_batch("INSERT INTO app_meta(key,value)VALUES('compact.result.compact','{\"kind\":\"compact\",\"state\":\"completed\"}')").unwrap();
        parity(&db, id);
    }
    #[test]
    fn eight_mib_outcome_is_not_copied_or_json_parsed_and_unavailable_never_fabricates_empty_targets()
     {
        let db = fixture();
        let body = json!({"result":{"body":"x".repeat(8*1024*1024-64)}}).to_string();
        insert(&db, "huge", "completed", Some(&body));
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        db.create_scalar_function(
            "json_valid",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            move |_| {
                observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(1i64)
            },
        )
        .unwrap();
        let raw = read_tx(&db, "huge", "current").unwrap();
        assert!(raw.body.is_none());
        assert_eq!(raw.body_bytes, Some(body.len() as u64));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "guard must prevent parsing an8MiB witness inside SQLite"
        );
        let prepared = prepare(raw).unwrap();
        assert_eq!(prepared.value["targets"], Value::Null);
        assert_eq!(prepared.value["downstream"]["available"], false);
        assert!(prepared.limited_fields.contains(&"outcome".into()));
    }
    #[test]
    fn aggregate_json_witness_budget_precedes_queue_membership_filtering() {
        let db = fixture();
        insert(
            &db,
            "compact",
            "completed",
            Some(
                "{\"result\":{\"status\":\"queued\",\"operation_id\":\"compact\",\"thread_id\":\"actual\"},\"padding\":\"small\"}",
            ),
        );
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('compact.pending.actual',?1)",
            [json!(["not-this-operation".repeat(JSON_BYTES)]).to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('compact.inflight.actual','[\"compact\"]')",
            [],
        )
        .unwrap();
        let raw = read_tx(&db, "compact", "current").unwrap();
        let Downstream::Compact {
            saved: _,
            pending,
            inflight,
        } = &raw.downstream
        else {
            panic!("compact witness")
        };
        assert!(pending.present && pending.body.is_none());
        assert!(
            inflight.present && inflight.body.is_none(),
            "oversized earlier witness exhausts the shared copy allowance before membership filtering"
        );
        let prepared = prepare(raw).unwrap();
        assert_eq!(prepared.value["downstream"]["available"], false);
        assert_eq!(prepared.value["downstream"]["state"], "unknown");
    }
    #[test]
    fn corrupt_saved_json_and_missing_receipt_are_explicit_errors() {
        let db = fixture();
        insert(&db, "bad", "completed", None);
        db.execute_batch("PRAGMA ignore_check_constraints=ON;UPDATE operation_receipt SET result_ref_json='{broken' WHERE operation_id='bad'").unwrap();
        assert!(prepare(read_tx(&db, "bad", "current").unwrap()).is_err());
        assert!(read_tx(&db, "missing", "current").is_err());
    }
    #[test]
    fn exact_pk_query_cost_and_statement_count_do_not_scale_with_unrelated_receipts() {
        let db = fixture();
        insert(&db, "selected", "pending", None);
        let baseline = {
            let trace = Statements::new(&db);
            let raw = read_tx(&db, "selected", "current").unwrap();
            assert_eq!(trace.count(), raw.statements);
            (raw.steps, raw.statements)
        };
        for size in [1000, 10000] {
            db.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<?1) INSERT OR IGNORE INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)SELECT 'unrelated:'||i,'old','null','hash','completed','{\"result\":{}}','now' FROM n",[size]).unwrap();
            let trace = Statements::new(&db);
            let raw = read_tx(&db, "selected", "current").unwrap();
            assert_eq!(
                (raw.steps, raw.statements),
                baseline,
                "exact PK observation must not walk a global prefix"
            );
            assert_eq!(trace.count(), raw.statements);
            assert!(raw.steps < 200 && raw.statements <= 2);
        }
    }
}
