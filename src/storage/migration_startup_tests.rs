use super::*;
use migration_integrity::{Check, pending, persist};

fn downgrade_copy(db: &Connection, version: i64) {
    db.execute_batch("DROP INDEX IF EXISTS jobs_running_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN archived_at;")
        .unwrap();
    if version < 26 {
        db.execute_batch("DROP INDEX IF EXISTS receipt_reply_pending;")
            .unwrap();
    }
    if version < 25 {
        db.execute_batch("DROP INDEX IF EXISTS history_tool_group_attempt;")
            .unwrap();
    }
    db.execute("DELETE FROM schema_migration WHERE version>?1", [version])
        .unwrap();
    db.pragma_update(None, "user_version", version).unwrap();
}

fn orphan(db: &Connection) {
    db.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO run(id,thread_id,state,created_at) VALUES('orphan','missing','completed','now');").unwrap();
}

#[test]
fn real_index_only_upgrade_skips_full_scan_and_clears_pending() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let db = open_connection(&path, None, None, true).unwrap();
    downgrade_copy(&db, 26);
    orphan(&db);
    drop(db);
    let db = open_connection(&path, None, None, true).unwrap();
    assert_eq!(pending(&db).unwrap(), None);
    assert!(
        Check::Full.run(&db).is_err(),
        "manual/full checks must still find unrelated corruption"
    );
}

#[test]
fn real_upgrade_without_metadata_checks_full_and_keeps_pending_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let db = open_connection(&path, None, None, true).unwrap();
    downgrade_copy(&db, 23); // Migration24 has no impact metadata.
    orphan(&db);
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Full));
    assert_eq!(schema_version(&db).unwrap(), SCHEMA_VERSION);
}

#[test]
fn scoped_pending_survives_current_version_reopen_and_checks_only_its_tables() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let db = open_connection(&path, None, None, true).unwrap();
    db.execute_batch("CREATE TABLE fk_parent(id INTEGER PRIMARY KEY); CREATE TABLE fk_child(parent_id REFERENCES fk_parent(id));").unwrap();
    orphan(&db);
    persist(&db, Check::Tables(["fk_child".into()].into())).unwrap();
    drop(db);
    let db = open_connection(&path, None, None, true).unwrap();
    assert_eq!(pending(&db).unwrap(), None);
    db.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO fk_child VALUES(123);")
        .unwrap();
    persist(&db, Check::Tables(["fk_child".into()].into())).unwrap();
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert!(matches!(pending(&db).unwrap(), Some(Check::Tables(_))));
}

#[test]
fn interrupted_upgrade_and_later_integrity_error_both_keep_pending() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let db = open_connection(&path, None, None, true).unwrap();
    downgrade_copy(&db, 26);
    db.execute_batch("CREATE TABLE jobs_running_recovery(id INTEGER);")
        .unwrap();
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Skip));
    assert_eq!(schema_version(&db).unwrap(), 26);
    db.execute_batch("DROP TABLE jobs_running_recovery;")
        .unwrap();
    drop(db);
    let db = open_connection(&path, None, None, true).unwrap();
    assert_eq!(pending(&db).unwrap(), None);
    persist(&db, Check::Skip).unwrap();
    db.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('corrupt','fixture','{}','hash','failed','malformed','now');").unwrap();
    drop(db);
    assert!(open_connection(&path, None, None, false).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Skip));
}

#[tokio::test]
#[ignore = "manual authorized closed-snapshot measurement; no owner or provider"]
async fn measure_startup_on_snapshot_copies() {
    let source = std::path::PathBuf::from(
        std::env::var_os("SHELLBOY_STARTUP_SNAPSHOT")
            .expect("provide a closed VACUUM INTO snapshot"),
    );
    for repetition in 1..=3 {
        for baseline in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("copy.db");
            std::fs::copy(&source, &path).unwrap();
            let db = Connection::open(&path).unwrap();
            assert_eq!(schema_version(&db).unwrap(), SCHEMA_VERSION);
            downgrade_copy(&db, 24);
            if baseline {
                // Preserve the previous full-check policy, with otherwise
                // identical migration/startup code and the same snapshot.
                db.execute(
                    "INSERT OR REPLACE INTO app_meta VALUES('integrity.migration_pending','1')",
                    [],
                )
                .unwrap();
            } else {
                db.execute(
                    "DELETE FROM app_meta WHERE key='integrity.migration_pending'",
                    [],
                )
                .unwrap();
            }
            drop(db);
            let diagnostics = crate::diagnostics::Diagnostics::start(
                temp.path().join("logs"),
                crate::config::Logging::default(),
            );
            let started = Instant::now();
            let db = open_connection(&path, None, Some(&diagnostics), true).unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            assert_eq!(pending(&db).unwrap(), None);
            drop(db);
            diagnostics.shutdown().await;
            let stages: Vec<serde_json::Value> =
                std::fs::read_to_string(temp.path().join("logs/shellboy.log"))
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .filter_map(|row| row.get("startup_stage").cloned())
                    .collect();
            println!(
                "STARTUP_SCOPE_MEASUREMENT={}",
                serde_json::json!({"repetition":repetition,"policy":if baseline {"previous_full"} else {"index_only_skip"},"database_open_seconds":elapsed,"stages":stages})
            );
        }
    }
}
