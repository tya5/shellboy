//! Exercise the actual history handler and exact final Response, not just the ledger helper.
use super::*;

#[tokio::test]
async fn history_handler_drops_only_relation_at_exact_frame_boundary() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('recipient','/','','fixture','fixture','2026-01-01T00:00:00Z');")?;Ok(())
    }).await.unwrap();
    let command:Command=serde_json::from_value(json!({"command":"message.send","args":{"thread_id":"recipient","text":"synthetic saved body","expect_reply":true}})).unwrap();
    let saved = control
        .store
        .execute(Request::new("saved-request", command), None)
        .await
        .unwrap()
        .result;
    let node = saved["message_id"].as_str().unwrap();
    let command: Command =
        serde_json::from_value(json!({"command":"history.get","args":{"node_id":node}})).unwrap();
    let (baseline, _) = control
        .evaluate(Request::new("x", command.clone()), CancellationToken::new())
        .await;
    assert!(baseline.ok);
    assert!(
        baseline.result.as_ref().unwrap()["provenance"]
            .get("message")
            .is_some()
    );
    let baseline_bytes = serde_json::to_vec(&baseline).unwrap().len();
    for delta in [-1i64, 0, 1] {
        let length = (crate::protocol::MAX_FRAME_BYTES - baseline_bytes + 1) as i64 + delta;
        let request = Request::new("x".repeat(length as usize), command.clone());
        request.validate().unwrap();
        assert!(serde_json::to_vec(&request).unwrap().len() <= crate::protocol::MAX_FRAME_BYTES);
        let (response, _) = control.evaluate(request, CancellationToken::new()).await;
        assert!(response.ok);
        let result = response.result.as_ref().unwrap();
        assert_eq!(result["provenance"].get("message").is_some(), delta <= 0);
        assert_eq!(
            result["provenance"]["sender"],
            baseline.result.as_ref().unwrap()["provenance"]["sender"]
        );
        assert_eq!(result["text"], baseline.result.as_ref().unwrap()["text"]);
        assert!(serde_json::to_vec(&response).unwrap().len() <= crate::protocol::MAX_FRAME_BYTES);
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
