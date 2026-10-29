//! Latest-Run facts drive exit markers; no real provider or user data.
use shellboy::{config::Config, control::db::Store, protocol::*, storage::DbWorker};
use std::sync::Arc;

#[tokio::test]
async fn thread_list_exposes_latest_stop_reason_and_new_run_clears_it() {
    let root = tempfile::tempdir().unwrap();
    let database = DbWorker::open(root.path().join("db")).await.unwrap();
    database.call(|db| {
        for (id, state, reason) in [
            ("crash", "stopped", Some("previous_instance_interrupted")),
            ("exit", "stopped", Some("app_shutdown")),
            ("explicit", "stopped", Some("explicit_stop")),
            ("completed", "completed", None),
            ("empty", "completed", None),
        ] {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?1,'/','/','p','m','now')",[id])?;
            if id != "empty" {
                db.execute("INSERT INTO run(id,thread_id,state,created_at,stop_reason) VALUES(?1,?1,?2,'old',?3)",rusqlite::params![id,state,reason])?;
            }
        }
        let plans = db.prepare("EXPLAIN QUERY PLAN SELECT stop_reason FROM run WHERE thread_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1")?.query_map(["crash"],|r|r.get::<_,String>(3))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(plans.iter().any(|p|p.contains("run_thread_created")),"{plans:?}");
        assert!(!plans.iter().any(|p|p.contains("TEMP B-TREE")),"{plans:?}");
        Ok(())
    }).await.unwrap();
    let store = Store::new(
        database,
        Arc::new(Config::default()),
        root.path().into(),
        "instance".into(),
    );
    let list = |id: &str| {
        Request::new(
            format!("instance:{id}"),
            Command::ThreadList(ThreadListArgs::default()),
        )
    };
    let first = store.execute(list("first"), None).await.unwrap().result;
    let first: Page<ThreadSummary> = serde_json::from_value(first).unwrap();
    for (id, reason) in [
        ("crash", Some("previous_instance_interrupted")),
        ("exit", Some("app_shutdown")),
        ("explicit", Some("explicit_stop")),
        ("completed", None),
        ("empty", None),
    ] {
        assert_eq!(
            first
                .items
                .iter()
                .find(|t| t.thread_id == id)
                .unwrap()
                .latest_run_stop_reason
                .as_deref(),
            reason
        );
    }
    store.database.call(|db| {
        // Same timestamp: ID is the existing deterministic latest-Run tie-break.
        db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('z-new','crash','live','old')",[])?;
        Ok(())
    }).await.unwrap();
    let next: Page<ThreadSummary> =
        serde_json::from_value(store.execute(list("next"), None).await.unwrap().result).unwrap();
    assert!(
        next.items
            .iter()
            .find(|t| t.thread_id == "crash")
            .unwrap()
            .latest_run_stop_reason
            .is_none()
    );
    store.database.shutdown().await.unwrap();
}
