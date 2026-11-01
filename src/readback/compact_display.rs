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
