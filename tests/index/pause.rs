//! Saved pause identity/time and per-Thread observation regressions.
use super::*;

async fn observe(f: &Fixture, service: Arc<IndexService>, at: &'static str) {
    f.db.call(move |db| {
        let tx = db.transaction()?;
        service.record_context_observations_tx(&tx, "pause-observer", at)?;
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
}
async fn observations(f: &Fixture) -> Vec<Value> {
    f.db.call(|db| {
        let mut q = db.prepare("SELECT payload_ref_json FROM event WHERE target_thread_id='pause-observer' ORDER BY created_at,id")?;
        Ok(q.query_map([], |r| r.get::<_, String>(0))?
            .map(|row| serde_json::from_str(&row.unwrap()).unwrap()).collect())
    }).await.unwrap()
}

#[tokio::test]
async fn pause_time_and_identity_survive_reads_service_restart_and_turn_observations() {
    let f = Fixture::new(Config::default()).await;
    f.db.call(|db| {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('pause-observer','/fixture','/fixture/AGENTS.md','embed','fixture','now')", [])?;
        Ok(())
    }).await.unwrap();
    f.node("paused-source", "cat synthetic paused source").await;
    f.service
        .enqueue_new(&["paused-source".into()])
        .await
        .unwrap();
    f.proxy.push(Reply::Error(401, "invalid_api_key"));
    assert!(
        f.service
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let first = f.service.status(args()).await.unwrap();
    assert!(first.paused);
    assert!(chrono::DateTime::parse_from_rfc3339(first.paused_at.as_deref().unwrap()).is_ok());
    assert!(first.pause_id.as_deref().is_some_and(|id| !id.is_empty()));
    observe(&f, f.service.clone(), "2026-10-30T00:00:00Z").await;
    let restarted = IndexService::new(f.db.clone(), f.config.clone());
    for at in ["2026-10-30T01:00:00Z", "2026-10-30T02:00:00Z"] {
        let status = restarted.status(args()).await.unwrap();
        assert_eq!(status.paused_at, first.paused_at);
        assert_eq!(status.pause_id, first.pause_id);
        observe(&f, restarted.clone(), at).await;
    }
    let saved = observations(&f).await;
    assert_eq!(saved.len(), 1, "unchanged pause must not repeat every Turn");
    assert_eq!(
        saved[0]["change"]["after"]["paused_at"],
        json!(first.paused_at)
    );
    assert_eq!(
        saved[0]["change"]["after"]["pause_id"],
        json!(first.pause_id)
    );
    assert_eq!(f.proxy.count(), 1, "observing a pause does not retry HTTP");

    let preview = restarted.preview(args()).await.unwrap();
    assert_eq!(preview.sources, 1);
    assert!(preview.characters > 0);
    assert_eq!(f.proxy.count(), 1, "retry preview remains read-only");
    restarted.retry(args()).await.unwrap();
    assert!(
        restarted
            .process_one(CancellationToken::new())
            .await
            .unwrap()
    );
    let resumed = restarted.status(args()).await.unwrap();
    assert!(!resumed.paused);
    assert!(resumed.paused_at.is_none() && resumed.pause_id.is_none());
    observe(&f, restarted, "2026-10-30T03:00:00Z").await;
    let saved_after = observations(&f).await;
    assert_eq!(saved_after.len(), 2);
    assert_eq!(
        saved_after[0], saved[0],
        "resumption does not rewrite history"
    );
    assert_eq!(saved_after[1]["change"]["after"]["paused"], false);
    assert_eq!(f.proxy.count(), 2);
    f.close().await;
}

#[tokio::test]
async fn historical_pause_without_saved_time_stays_unknown_and_stable() {
    let f = Fixture::new(Config::default()).await;
    f.node("legacy-paused-source", "cat").await;
    f.service
        .enqueue_new(&["legacy-paused-source".into()])
        .await
        .unwrap();
    f.proxy.push(Reply::Error(401, "invalid_api_key"));
    f.service
        .process_one(CancellationToken::new())
        .await
        .unwrap();
    f.db.call(|db| {
        db.execute("UPDATE app_meta SET value=json_remove(value,'$.paused_at') WHERE key LIKE 'index.pause.%'", [])?;
        Ok(())
    }).await.unwrap();
    let before = f.service.status(args()).await.unwrap();
    let after = IndexService::new(f.db.clone(), f.config.clone())
        .status(args())
        .await
        .unwrap();
    assert!(before.paused && after.paused);
    assert!(before.paused_at.is_none() && after.paused_at.is_none());
    assert_eq!(before.pause_id, after.pause_id);
    assert_eq!(f.proxy.count(), 1);
    f.close().await;
}
