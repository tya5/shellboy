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
    // Store has no owner admission loop: select the saved body explicitly in this fixture.
    let selected = node.to_owned();
    store.database.call(move|db|{db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) SELECT 'recipient',COALESCE(MAX(ordinal)+1,0),?1 FROM thread_path WHERE thread_id='recipient'",[selected])?;Ok(())}).await.unwrap();
    let command:Command=serde_json::from_value(serde_json::json!({"command":"history.window","args":{"thread_id":"recipient","items":true}})).unwrap();
    let window = store.execute(request(command), None).await.unwrap().result;
    assert!(window["items"].as_array().unwrap().iter().any(
        |item| item["node_id"] == node && item["provenance"]["message"]["expect_reply"] == true
    ));
}

#[tokio::test]
async fn human_reply_only_reverse_relation_is_exact_across_unrelated_send() {
    let (_root, store) = fixture().await;
    let accepted = store
        .execute(request(send(true)), None)
        .await
        .unwrap()
        .result;
    let node = accepted["message_id"].as_str().unwrap().to_owned();
    store.execute(request(send(false)), None).await.unwrap();
    let admitted = node.clone();
    store.database.call(move|db|{db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1",[admitted])?;Ok(())}).await.unwrap();
    let result = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: node.clone(),
                text: "synthetic human reply".into(),
                sender_label: Some("claimed unrelated title".into()),
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    let reply = result["message_id"].as_str().unwrap();
    let command: Command = serde_json::from_value(
        serde_json::json!({"command":"history.get","args":{"node_id":reply}}),
    )
    .unwrap();
    let body = store.execute(request(command), None).await.unwrap().result;
    assert_eq!(body["provenance"]["message"]["reply_to"], node);
    assert_eq!(body["provenance"]["message"]["expect_reply"], false);
    assert!(
        body["text"]
            .as_str()
            .unwrap()
            .contains("synthetic human reply")
    );
    store.database.shutdown().await.unwrap();
}
