//! WAL readers must not turn a clean writer close into a save failure.
use rusqlite::{Connection, OpenFlags};
use shellboy::storage::DbWorker;
use std::time::Duration;

struct CheckpointCount {
    db: DbWorker,
    start: usize,
}
impl CheckpointCount {
    fn checkpoint_attempts(&self) -> usize {
        self.db.checkpoint_attempts().saturating_sub(self.start)
    }
}

async fn observe_checkpoints(
    db: &DbWorker,
    initialize: impl FnOnce(&Connection) -> rusqlite::Result<()> + Send + 'static,
) -> CheckpointCount {
    let count = CheckpointCount {
        db: db.clone(),
        start: db.checkpoint_attempts(),
    };
    db.call(move |connection| {
        initialize(connection)?;
        Ok(())
    })
    .await
    .unwrap();
    count
}

async fn checkpoint_barrier(db: &DbWorker, _count: &CheckpointCount) {
    read_job(db).await;
}

async fn read_job(db: &DbWorker) {
    db.call(|connection| {
        assert_eq!(
            connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?,
            1
        );
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn read_only_idle_jobs_do_not_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("fixture.db"))
        .await
        .unwrap();
    let count = observe_checkpoints(&db, |_| Ok(())).await;
    for _ in 0..64 {
        read_job(&db).await;
        // Let the dedicated actor finish the preceding idle boundary.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let attempts = count.checkpoint_attempts();
    checkpoint_barrier(&db, &count).await;
    db.shutdown().await.unwrap();
    assert_eq!(
        attempts, 0,
        "read-only jobs caused {attempts} idle checkpoints"
    );
}

async fn wait_for_attempt(db: &DbWorker, count: &CheckpointCount, expected: usize) {
    let observed =
        crate::test_watchdog::watchdog("tests/storage_checkpoint.rs: wait_for_attempt", async {
            while count.checkpoint_attempts() < expected {
                // Checkpoints run only at idle opportunities, never on a timer.
                // Keep driving the actor rather than assuming a fixed sleep made
                // one particular read job eligible on every scheduler.
                read_job(db).await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    assert!(
        observed.is_ok(),
        "idle checkpoint not observed: expected {expected}, actual {}",
        count.checkpoint_attempts()
    );
    // A queued job is a barrier for the previous idle boundary.
    read_job(db).await;
    assert_eq!(count.checkpoint_attempts(), expected);
}

#[tokio::test]
async fn partial_checkpoint_retries_after_interval_then_clean_reads_stay_idle() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.db");
    let db = DbWorker::open(&path).await.unwrap();
    db.call(|writer| {
        assert_eq!(
            writer.query_row("PRAGMA wal_autocheckpoint", [], |row| row.get::<_, i64>(0))?,
            1000
        );
        writer.execute(
            "INSERT INTO app_meta VALUES('checkpoint-fixture','old')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let reader = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row(
                "SELECT value FROM app_meta WHERE key='checkpoint-fixture'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "old"
    );
    // Make the update's own idle boundary eligible and start observing first.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let count = observe_checkpoints(&db, |writer| {
        writer.execute(
            "UPDATE app_meta SET value='new' WHERE key='checkpoint-fixture'",
            [],
        )?;
        Ok(())
    })
    .await;
    wait_for_attempt(&db, &count, 1).await;
    for _ in 0..32 {
        read_job(&db).await;
    }
    assert_eq!(
        count.checkpoint_attempts(),
        1,
        "blocked reader must not cause a retry per job"
    );
    reader.execute_batch("ROLLBACK").unwrap();
    reader.close().unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    read_job(&db).await;
    wait_for_attempt(&db, &count, 2).await;
    // Retry must have retained the pending write even without another mutation;
    // after full recovery the same read jobs must no longer trigger attempts.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    for _ in 0..8 {
        read_job(&db).await;
        // Keep idle opportunities observable before the barrier.
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(count.checkpoint_attempts(), 2);
    checkpoint_barrier(&db, &count).await;
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_with_an_old_read_snapshot_preserves_committed_wal() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.db");
    let db = DbWorker::open(&path).await.unwrap();
    db.call(|writer| {
        writer.execute(
            "INSERT INTO app_meta VALUES('checkpoint-fixture','old')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let reader = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let old: String = reader
        .query_row(
            "SELECT value FROM app_meta WHERE key='checkpoint-fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old, "old");
    db.call(|writer| {
        writer.execute(
            "UPDATE app_meta SET value='committed' WHERE key='checkpoint-fixture'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    // The old snapshot prevents TRUNCATE, but the committed WAL is still durable.
    let result = db.shutdown().await;
    let reader_value: String = reader
        .query_row(
            "SELECT value FROM app_meta WHERE key='checkpoint-fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reader_value, "old");
    reader.execute_batch("ROLLBACK").unwrap();
    reader.close().unwrap();
    result.expect("a reader's checkpoint contention is not a save failure");
    let reopened = DbWorker::open(&path).await.unwrap();
    reopened
        .call(|writer| {
            let value: String = writer.query_row(
                "SELECT value FROM app_meta WHERE key='checkpoint-fixture'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(value, "committed");
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}
