//! Short-lived display snapshots on the existing finite presentation pool.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::{Connection, OpenFlags};
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

pub(super) fn storage_error(error: StorageError) -> ApiError {
    if let StorageError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) = &error
        && matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy
                | rusqlite::ErrorCode::DatabaseLocked
                | rusqlite::ErrorCode::OperationInterrupted
        )
    {
        return ApiError::new(
            ErrorCode::InvalidState,
            "Display snapshot is temporarily unavailable",
        )
        .hint("Refresh after pending database work completes");
    }
    super::storage_error(error)
}

/// Order visible observations before their asynchronous snapshot acquisition.
/// A late old snapshot must not mint a token newer than an overtaking history read.
pub(super) async fn read_sequenced<T: Send + 'static>(
    path: PathBuf,
    stop: CancellationToken,
    collect: impl FnOnce(&Connection, u64) -> Result<T, StorageError> + Send + 'static,
) -> Result<(T, u64), ApiError> {
    let sequence = super::message_queue::read_revision();
    read(path, stop, move |db| {
        collect(db, sequence).map(|raw| (raw, sequence))
    })
    .await
}

pub(super) async fn read<T: Send + 'static>(
    path: PathBuf,
    stop: CancellationToken,
    collect: impl FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
) -> Result<T, ApiError> {
    if stop.is_cancelled() {
        return Err(closed());
    }
    super::presentation_work(move || {
        let result = (|| {
            if stop.is_cancelled() {
                return Err(closed().into());
            }
            let mut db = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            db.busy_timeout(Duration::from_millis(250))?;
            db.pragma_update(None, "query_only", true)?;
            let transaction = db.transaction()?;
            // Establish the WAL snapshot at this typed boundary, before domain
            // helpers can wrap a lock error into a saved-result API error.
            let _: i64 = transaction.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
            let raw = collect(&transaction)?;
            transaction.commit()?;
            Ok(raw)
        })();
        result.map_err(storage_error)
    })
    .await?
}

fn closed() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidState,
        "Display snapshot observation closed",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DbWorker;
    use tokio::{sync::oneshot, time::timeout};

    #[tokio::test]
    async fn display_snapshot_and_browser_facts_do_not_wait_for_writer_queue() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("display.sqlite");
        let writer = DbWorker::open(&path).await.unwrap();
        writer.call(|db| {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('root','','','','','','now')", [])?;
            Ok(())
        }).await.unwrap();
        let (entered, ready) = oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let pending = writer
            .try_call(move |_| {
                let _ = entered.send(());
                let _ = blocked.recv();
                Ok(())
            })
            .unwrap();
        timeout(Duration::from_secs(60), ready)
            .await
            .unwrap()
            .unwrap();
        let result = timeout(
            Duration::from_secs(60),
            read(path, CancellationToken::new(), |db| {
                assert_eq!(
                    db.pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))?,
                    1
                );
                crate::frontend::browser_read::read_tx(db, &["root".into()], None)
            }),
        )
        .await;
        drop(release);
        pending.receive().await.unwrap();
        assert_eq!(
            crate::frontend::browser_read::prepare(result.unwrap().unwrap())
                .rows
                .len(),
            1
        );
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn multiple_display_consumers_share_one_wal_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("display.sqlite");
        let writer = DbWorker::open(&path).await.unwrap();
        writer
            .call(|db| {
                db.execute(
                    "INSERT INTO app_meta(key,value)VALUES('display-test','before')",
                    [],
                )?;
                db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('root','','','','','','now')", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let (entered, ready) = oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let reader = tokio::spawn(read_sequenced(
            path,
            CancellationToken::new(),
            move |db, _sequence| {
                let first: String = db.query_row(
                    "SELECT value FROM app_meta WHERE key='display-test'",
                    [],
                    |row| row.get(0),
                )?;
                let _ = entered.send(());
                let _ = blocked.recv();
                let second: String = db.query_row(
                    "SELECT value FROM app_meta WHERE key='display-test'",
                    [],
                    |row| row.get(0),
                )?;
                Ok((first, second))
            },
        ));
        timeout(Duration::from_secs(60), ready)
            .await
            .unwrap()
            .unwrap();
        let changed = writer
            .call(|db| {
                let transaction = db.transaction()?;
                transaction.execute(
                    "UPDATE app_meta SET value='after' WHERE key='display-test'",
                    [],
                )?;
                crate::history::append_node(
                    &transaction,
                    "root",
                    "new-answer",
                    None,
                    "assistant_response",
                    &serde_json::json!({"text":"synthetic later answer"}),
                    "2026-01-01T00:00:00Z",
                )?;
                let (raw, _) = crate::frontend::history_reader::query_page_tx(
                    &transaction,
                    "root",
                    None,
                    None,
                )?;
                let sequence = raw.info().read_revision;
                assert_eq!(raw.info().total, 1);
                transaction.commit()?;
                Ok(sequence)
            })
            .await;
        drop(release);
        let newer_page_sequence = changed.unwrap();
        let (observed, older_sequence) = timeout(Duration::from_secs(60), reader)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(observed, ("before".into(), "before".into()));
        assert!(
            older_sequence < newer_page_sequence,
            "old WAL snapshot completion must not overtake a newer canonical history read"
        );
        writer.shutdown().await.unwrap();
    }

    #[test]
    fn transient_read_errors_do_not_latch_a_writer_failure() {
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_INTERRUPT,
        ] {
            let error = storage_error(StorageError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            )));
            assert_eq!(error.code, ErrorCode::InvalidState);
            assert!(!error.is_storage_failure());
        }
        assert!(
            storage_error(StorageError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                None
            )))
            .is_storage_failure()
        );
    }
    #[tokio::test]
    async fn cancelled_display_never_opens_an_invalid_path() {
        let stop = CancellationToken::new();
        stop.cancel();
        let error = read_sequenced(
            PathBuf::from("missing-parent/missing.sqlite"),
            stop,
            |_, _| Ok(()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(!error.is_storage_failure());
        assert!(error.message.contains("closed"));
    }

    #[tokio::test]
    async fn later_query_contention_keeps_its_retryable_type() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("display.sqlite");
        let writer = DbWorker::open(&path).await.unwrap();
        let result: Result<(), ApiError> = read(path, CancellationToken::new(), |_| {
            Err(StorageError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            )))
        })
        .await;
        let error = result.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(!error.is_storage_failure());
        writer.shutdown().await.unwrap();
    }
}
