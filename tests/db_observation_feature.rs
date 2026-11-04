use shellboy::storage::{DbWorker, StorageError};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn integration_worker_samples_fast_statements() {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = db.clone();
    let task = tokio::spawn(async move {
        worker
            .call(move |connection| {
                let value: i64 = connection.query_row("SELECT 1", [], |row| row.get(0))?;
                assert_eq!(value, 1);
                let _ = started_tx.send(());
                release_rx.recv().unwrap();
                Ok::<_, StorageError>(())
            })
            .await
    });
    started_rx.await.unwrap();
    let (steps, sample_all) = db.test_active_statement_counters().unwrap();
    assert!(steps > 0);
    assert!(sample_all, "integration worker must sample every profile");
    release_tx.send(()).unwrap();
    task.await.unwrap().unwrap();
    db.shutdown().await.unwrap();
}
