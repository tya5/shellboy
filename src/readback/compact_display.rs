//! Indexed latest-compaction display; canonical CLI reads remain exact.
use super::*;
use crate::storage::StorageError;
pub(crate) fn read(
    db: &Connection,
    thread: &str,
) -> Result<Option<compact::RawCompactions>, ApiError> {
    if thread.is_empty() || thread.len() > 512 {
        return Err(invalid("Invalid Thread identity for compaction display"));
    }
    match compact::read_display(db, thread) {
        Ok(raw) => Ok(Some(raw)),
        Err(StorageError::Operation(error)) if error.code == ErrorCode::InputLimit => Ok(None),
        Err(error) => Err(compact::capture_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(padding: usize) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now')",[]).unwrap();
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('op','fixture','{}','hash','completed',?1,'2026-10-31T00:00:00Z')",
            [json!({"result":{"thread_id":"t","status":"queued","context_edit":{"action":"restore"}}}).to_string()]).unwrap();
        db.execute("INSERT INTO app_meta(key,value)VALUES('compact.result.op',?1)",
            [json!({"state":"completed","ended_at":"2026-10-31T00:01:00Z","outcome":{"result":{"omitted_nodes":0,"opaque":"x".repeat(padding)}}}).to_string()]).unwrap();
        db
    }
    fn args() -> CompactListArgs {
        CompactListArgs {
            thread_id: Some("t".into()),
            since: None,
            until: None,
            limit: 1,
        }
    }
    fn length_limit(db: &Connection) -> i32 {
        // SAFETY: querying this owned test connection changes no limit.
        unsafe { rusqlite::ffi::sqlite3_limit(db.handle(), rusqlite::ffi::SQLITE_LIMIT_LENGTH, -1) }
    }
    fn prove_restored(db: &Connection, old: i32) {
        assert_eq!(length_limit(db), old);
        let large = "x".repeat(1024 * 1024);
        let value: String = db
            .query_row("SELECT ?1", [&large], |row| row.get(0))
            .unwrap();
        assert_eq!(value.len(), large.len());
        // More work than the scoped hook allows: a surviving hook would abort.
        let sum:i64=db.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) SELECT sum(x) FROM n",[],|row|row.get(0)).unwrap();
        assert_eq!(sum, 5_000_050_000);
        assert!(
            !db.is_autocommit(),
            "display interruption must not discard the shared snapshot"
        );
    }
    #[test]
    fn complete_display_matches_canonical_and_restores_connection_before_shared_snapshot_continues()
    {
        let db = fixture(0);
        let expected = compact::list(&db, "t", &args()).unwrap();
        let old = length_limit(&db);
        db.execute_batch("BEGIN").unwrap();
        let raw = read(&db, "t").unwrap().unwrap();
        prove_restored(&db, old);
        db.execute_batch("ROLLBACK").unwrap();
        drop(db);
        let displayed = compact::prepare(raw).unwrap();
        assert_eq!(displayed["items"], expected["items"]);
        assert_eq!(displayed["coverage"]["source_candidate_limit"], 201);
    }
    #[test]
    fn indexed_display_keeps_shared_snapshot_and_future_queries_usable() {
        let db = fixture(0);
        let old = length_limit(&db);
        db.execute_batch("BEGIN").unwrap();
        assert!(read(&db, "t").unwrap().is_some());
        prove_restored(&db, old);
        assert_eq!(
            compact::list(&db, "t", &args()).unwrap()["items"][0]["kind"],
            "restore"
        );
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn oversized_source_and_scalar_are_unknown_only_in_display_and_never_rewrite_cli_data() {
        for padding in [1024 * 1024, 0] {
            let db = fixture(padding);
            if padding == 0 {
                let id = "i".repeat(513);
                db.execute("UPDATE operation_receipt SET operation_id=?1", [&id])
                    .unwrap();
                db.execute(
                    "UPDATE app_meta SET key=?1 WHERE key='compact.result.op'",
                    [format!("compact.result.{id}")],
                )
                .unwrap();
            }
            let expected = compact::list(&db, "t", &args()).unwrap();
            let old = length_limit(&db);
            db.execute_batch("BEGIN").unwrap();
            assert!(read(&db, "t").unwrap().is_none());
            prove_restored(&db, old);
            assert_eq!(compact::list(&db, "t", &args()).unwrap(), expected);
            assert_eq!(
                db.query_row("SELECT count(*) FROM operation_receipt", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            db.execute_batch("ROLLBACK").unwrap();
        }
    }
    #[test]
    fn unrelated_storage_errors_are_not_hidden_as_a_display_limit() {
        let db = fixture(0);
        db.execute_batch("DROP TABLE app_meta; BEGIN").unwrap();
        let old = length_limit(&db);
        assert!(matches!(read(&db,"t"),Err(error) if error.code==ErrorCode::SaveFailed));
        prove_restored(&db, old);
        db.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn aggregate_copy_budget_rejects_individually_legal_metrics_before_string_copies() {
        let db = fixture(0);
        let meta=json!({"state":"completed","ended_at":"2026-10-31T00:01:00Z","outcome":{"result":{"marker_node_id":"m".repeat(70000),"omitted_nodes":0}}}).to_string();
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='compact.result.op'",
            [&meta],
        )
        .unwrap();
        for i in 1..5 {
            let id = format!("op{i}");
            db.execute("INSERT INTO operation_receipt SELECT ?1,instance_id,origin_json,request_hash,state,result_ref_json,created_at,reserved_bytes FROM operation_receipt WHERE operation_id='op'",[&id]).unwrap();
            db.execute(
                "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
                params![format!("compact.result.{id}"), meta],
            )
            .unwrap();
        }
        let error = compact::collect_tx(&db, "t", &args(), &mut compact::CopyBudget::display())
            .err()
            .expect("aggregate must exceed256KiB");
        assert!(
            matches!(error,StorageError::Operation(error) if error.code==ErrorCode::InputLimit)
        );
        assert!(read(&db, "t").unwrap().is_none());
        assert_eq!(
            compact::list(&db, "t", &args()).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;
    use crate::protocol::CompactListArgs;

    #[test]
    fn terminal_outcome_is_identical_in_canonical_and_display_and_quarantines_only_bad_fact() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','2026-11-01T00:00:00Z');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2026-11-01T00:00:00Z');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','{}','2026-11-01T00:00:00Z');").unwrap();
        let fact = json!({"phase":"closed_summary","elapsed_ms":17,"outcome":{"kind":"failed","reason":"provider_error","origin":"provider_response","http_status":401}});
        let mut usage = crate::compaction::SummaryUsage {
            requests: 1,
            ..Default::default()
        };
        usage.input_tokens.unknown_count = 1;
        usage.cached_tokens.unknown_count = 1;
        usage.cache_creation_tokens.unknown_count = 1;
        usage.output_tokens.unknown_count = 1;
        let usage = serde_json::to_value(usage).unwrap();
        let args = CompactListArgs {
            thread_id: Some("t".into()),
            since: None,
            until: None,
            limit: 1,
        };
        for malformed in [false, true] {
            let mut saved = fact.clone();
            if malformed {
                saved["outcome"]["provider_code"] = json!("SECRET");
            }
            let body=json!({"independent":"SECRET","automatic_trim_attempt":{"outcome":"failed","reason":"proactive_threshold","compaction_outcome":saved,"discard":{"reason":"other","phase":"closed_summary","elapsed_ms":1},"error":{"details":{"summary_usage":usage,"provider_code":"SECRET"}}}}).to_string();
            db.execute(
                "UPDATE model_attempt SET error_json=?1 WHERE id='a'",
                [&body],
            )
            .unwrap();
            let canonical = compact::list(&db, "t", &args).unwrap();
            let display = compact::prepare(read(&db, "t").unwrap().unwrap()).unwrap();
            assert_eq!(display["items"], canonical["items"]);
            let item = &canonical["items"][0];
            assert_eq!(item["attempt_id"], "a");
            assert_eq!(item["reason"], "proactive_threshold");
            assert_eq!(item["state"], "failed");
            assert_eq!(item["metrics"]["summary_usage"], usage);
            if malformed {
                assert!(item.get("compaction_outcome").is_none());
            } else {
                assert_eq!(item["compaction_outcome"], fact);
            }
            assert!(!canonical.to_string().contains("SECRET"));
            assert!(!display.to_string().contains("SECRET"));
            assert_eq!(
                db.query_row(
                    "SELECT error_json FROM model_attempt WHERE id='a'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                body
            );
        }
        let historical=json!({"automatic_trim_attempt":{"outcome":"failed","discard":{"reason":"other","phase":"closed_summary","elapsed_ms":3}}}).to_string();
        db.execute("UPDATE model_attempt SET error_json=?1", [&historical])
            .unwrap();
        let item = compact::list(&db, "t", &args).unwrap();
        assert!(item["items"][0].get("compaction_outcome").is_none());
        assert_eq!(
            db.query_row("SELECT error_json FROM model_attempt", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            historical
        );
    }
}
