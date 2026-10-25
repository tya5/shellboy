//! WAL readers must not turn a clean writer close into a save failure.
use rusqlite::{Connection, OpenFlags};
use shellboy::storage::DbWorker;
use std::{
    ffi::{CStr, c_void},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

unsafe extern "C" fn count_passive(
    mask: u32,
    context: *mut c_void,
    statement: *mut c_void,
    _: *mut c_void,
) -> i32 {
    if mask == rusqlite::ffi::SQLITE_TRACE_PROFILE && !statement.is_null() {
        // SQLite retains the registered Arc until the callback is unregistered.
        let sql = unsafe { CStr::from_ptr(rusqlite::ffi::sqlite3_sql(statement.cast())) };
        if sql
            .to_bytes()
            .eq_ignore_ascii_case(b"PRAGMA wal_checkpoint(PASSIVE)")
        {
            unsafe { &*context.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
        }
    }
    0
}

async fn trace_checkpoints(db: &DbWorker) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let retained = Arc::into_raw(count.clone()) as usize;
    db.call(move |connection| {
        // SAFETY: retained owns an Arc; the callback only accesses its atomic.
        assert_eq!(
            unsafe {
                rusqlite::ffi::sqlite3_trace_v2(
                    connection.handle(),
                    rusqlite::ffi::SQLITE_TRACE_PROFILE,
                    Some(count_passive),
                    retained as *mut c_void,
                )
            },
            rusqlite::ffi::SQLITE_OK
        );
        Ok(())
    })
    .await
    .unwrap();
    // The fixture unregisters before shutdown; on panic leaking the retained Arc
    // is safer than leaving SQLite with a dangling callback context.
    count
}

async fn stop_trace(db: &DbWorker, count: &Arc<AtomicUsize>) {
    let retained = Arc::as_ptr(count) as usize;
    db.call(move |connection| {
        assert_eq!(
            unsafe {
                rusqlite::ffi::sqlite3_trace_v2(connection.handle(), 0, None, std::ptr::null_mut())
            },
            rusqlite::ffi::SQLITE_OK
        );
        unsafe {
            drop(Arc::from_raw(retained as *const AtomicUsize));
        }
        Ok(())
    })
    .await
    .unwrap();
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
    let count = trace_checkpoints(&db).await;
    for _ in 0..64 {
        read_job(&db).await;
        // Let the dedicated actor finish the preceding idle boundary.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let attempts = count.load(Ordering::SeqCst);
    stop_trace(&db, &count).await;
    db.shutdown().await.unwrap();
    assert_eq!(
        attempts, 0,
        "read-only jobs caused {attempts} idle checkpoints"
    );
}

async fn wait_for_attempt(count: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while count.load(Ordering::SeqCst) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("an eligible idle job must checkpoint");
    assert_eq!(count.load(Ordering::SeqCst), expected);
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
    db.call(|writer| {
        writer.execute(
            "UPDATE app_meta SET value='new' WHERE key='checkpoint-fixture'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let count = trace_checkpoints(&db).await;
    // This sleep creates an eligible boundary; the actual attempt is observed
    // through SQLite, not inferred from the reply (which precedes idle work).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    read_job(&db).await;
    wait_for_attempt(&count, 1).await;
    for _ in 0..32 {
        read_job(&db).await;
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "blocked reader must not cause a retry per job"
    );
    reader.execute_batch("ROLLBACK").unwrap();
    reader.close().unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    read_job(&db).await;
    wait_for_attempt(&count, 2).await;
    // Retry must have retained the pending write even without another mutation;
    // after full recovery the same read jobs must no longer trigger attempts.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    for _ in 0..8 {
        read_job(&db).await;
        // Keep idle opportunities observable before unregistering the tracer.
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(count.load(Ordering::SeqCst), 2);
    stop_trace(&db, &count).await;
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
