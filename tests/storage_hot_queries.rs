use rusqlite::Connection;
use shellboy::storage::{DbWorker, StorageError};

const DROP_HOT_INDEXES: &str = "DROP INDEX shell_active_owner; DROP INDEX run_thread_created; DROP INDEX shell_terminal_time; DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DELETE FROM schema_migration WHERE version>=10; PRAGMA user_version=9;";

fn schema(db: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
    db.prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

#[tokio::test]
async fn hot_query_upgrade_matches_fresh_schema_and_preserves_data() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("upgrade.sqlite");
    let worker = DbWorker::open(&path).await.unwrap();
    worker.call(|db| {
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('saved','preserve','/','/AGENTS.md','p','m','now')", [])?;
        Ok(())
    }).await.unwrap();
    worker.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch(DROP_HOT_INDEXES).unwrap();
    db.close().unwrap();
    let worker = DbWorker::open(&path).await.unwrap();
    let upgraded = worker
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT title FROM thread WHERE id='saved'", [], |r| r
                    .get::<_, String>(0))?,
                "preserve"
            );
            assert_eq!(
                db.query_row(
                    "SELECT name FROM schema_migration WHERE version=10",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "0010_hot_query_indexes"
            );
            Ok(schema(db)?)
        })
        .await
        .unwrap();
    worker.shutdown().await.unwrap();
    let fresh = DbWorker::open(root.path().join("fresh.sqlite"))
        .await
        .unwrap();
    assert_eq!(fresh.call(|db| Ok(schema(db)?)).await.unwrap(), upgraded);
    fresh.shutdown().await.unwrap();
    let reopened = DbWorker::open(&path).await.unwrap();
    assert_eq!(reopened.call(|db| Ok(schema(db)?)).await.unwrap(), upgraded);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn hot_query_index_failure_rolls_back_indexes_and_version_together() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollback.sqlite");
    let worker = DbWorker::open(&path).await.unwrap();
    worker.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch(DROP_HOT_INDEXES).unwrap();
    db.execute("CREATE TABLE run_thread_created(blocker INTEGER)", [])
        .unwrap();
    db.close().unwrap();
    assert!(matches!(
        DbWorker::open(&path).await,
        Err(StorageError::Sqlite(_))
    ));
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM schema_migration WHERE version=10",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE type='index' AND name IN ('shell_active_owner','shell_terminal_time')", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    db.execute("DROP TABLE run_thread_created", []).unwrap();
    db.close().unwrap();
    let worker = DbWorker::open(&path).await.unwrap();
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn hot_query_plans_use_expected_access_paths() {
    let root = tempfile::tempdir().unwrap();
    let worker = DbWorker::open(root.path().join("plans.sqlite"))
        .await
        .unwrap();
    worker.call(|db| {
        for (sql, expected) in [
            ("SELECT id,owner_run_id,turn_id FROM shell_execution WHERE state IN ('starting','running','stopping')", "shell_active_owner"),
            ("SELECT COUNT(*) FROM shell_execution WHERE owner_run_id='run' AND state IN ('starting','running','stopping')", "shell_active_owner"),
            ("SELECT id FROM run WHERE thread_id='thread' ORDER BY created_at DESC,id DESC LIMIT 1", "run_thread_created"),
            ("SELECT id FROM run WHERE thread_id='thread' AND state IN ('live','stopping') AND state='live'", "one_live_run"),
            ("SELECT id FROM shell_execution WHERE state IN ('exited','failed','stopped') AND COALESCE(ended_at,started_at)>'now' ORDER BY COALESCE(ended_at,started_at),rowid LIMIT 128", "shell_terminal_time"),
        ] {
            let plan = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?.query_map([], |r| r.get::<_, String>(3))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(plan.iter().any(|line| line.contains(expected)), "expected {expected}: {plan:?}");
        }
        Ok(())
    }).await.unwrap();
    worker.shutdown().await.unwrap();
}
