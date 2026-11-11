use shellboy::{config::Config, control::db::Store, protocol::*, storage::DbWorker};
use std::sync::Arc;

async fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("reply.db")).await.unwrap();
    db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('recipient','/','/a','p','m','0'),('sender','/','/a','p','m','0');")?;
        Ok(())
    }).await.unwrap();
    let store = Store::new(
        db,
        Arc::new(Config::default()),
        root.path().into(),
        "fixture".into(),
    );
    (root, store)
}
fn request(command: Command) -> Request {
    Request::new(format!("fixture:{}", uuid::Uuid::new_v4()), command)
}
fn send(expected: bool) -> Command {
    Command::MessageSend(MessageSendArgs {
        resume_recovery: false,

        human: false,
        to_human: false,
        expect_reply: expected,
        sender_label: None,
        thread_id: Some("recipient".into()),
        text: "synthetic request".into(),
        deliver: Deliver::Next,
    })
}
#[tokio::test]
async fn saved_request_relation_survives_replay_and_window_read() {
    let (_root, store) = fixture().await;
    let operation = request(send(true));
    let first = store.execute(operation.clone(), None).await.unwrap().result;
    let replay = store.execute(operation, None).await.unwrap().result;
    assert_eq!(first, replay);
    let node = first["message_id"].as_str().unwrap();
    let command: Command = serde_json::from_value(
        serde_json::json!({"command":"history.get","args":{"node_id":node}}),
    )
    .unwrap();
    let body = store.execute(request(command), None).await.unwrap().result;
    assert_eq!(body["provenance"]["message"]["expect_reply"], true);
    assert_eq!(body["provenance"]["message"]["reply_state"], "pending");
    assert!(body["text"].as_str().unwrap().contains("synthetic request"));
    let command:Command=serde_json::from_value(serde_json::json!({"command":"history.window","args":{"thread_id":"recipient","items":true}})).unwrap();
    let window = store.execute(request(command), None).await.unwrap().result;
    assert!(window["items"].as_array().unwrap().iter().any(
        |item| item["node_id"] == node && item["provenance"]["message"]["expect_reply"] == true
    ));
}
