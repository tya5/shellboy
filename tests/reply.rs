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
        expect_reply: expected,
        sender_label: None,
        thread_id: Some("recipient".into()),
        text: "synthetic request".into(),
        deliver: Deliver::Next,
    })
}
#[tokio::test]
async fn expected_reply_cap_is_128_and_replay_is_not_a_new_duty() {
    let (_root, store) = fixture().await;
    let first = request(send(true));
    let accepted = store.execute(first.clone(), None).await.unwrap().result;
    assert_eq!(accepted["expect_reply"], true);
    for _ in 1..128 {
        store.execute(request(send(true)), None).await.unwrap();
    }
    assert_eq!(
        store.execute(first.clone(), None).await.unwrap().result,
        accepted
    );
    let mut conflict = first;
    conflict.command = send(false);
    assert_eq!(
        store.execute(conflict, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    assert_eq!(
        store
            .execute(request(send(true)), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InputLimit
    );
    store.execute(request(send(false)), None).await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn reply_requires_applied_pending_and_is_idempotent() {
    let (_root, store) = fixture().await;
    let accepted = store
        .execute(request(send(true)), None)
        .await
        .unwrap()
        .result;
    let id = accepted["message_id"].as_str().unwrap().to_owned();
    let reply = request(Command::MessageReply(MessageReplyArgs {
        message_id: id.clone(),
        text: "synthetic reply".into(),
        sender_label: None,
    }));
    let before = store.execute(reply.clone(), None).await.unwrap_err();
    assert_eq!(before.code, ErrorCode::InvalidState);
    let admitted = id.clone();
    store.database.call(move |db| { db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&admitted])?; Ok(()) }).await.unwrap();
    let result = store.execute(reply.clone(), None).await.unwrap().result;
    assert_eq!(result["reply_to"], id);
    assert_eq!(store.execute(reply, None).await.unwrap().result, result);
    let details = store
        .execute(
            request(Command::MessageGet(MessageTarget {
                message_id: id.clone(),
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(details["expect_reply"], true);
    assert_eq!(details["reply"]["state"], "replied");
    assert_eq!(details["reply"]["node_id"], result["message_id"]);
    let late = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: id,
                text: "late".into(),
                sender_label: None,
            })),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(late.details["reply_state"], "replied");
    store.database.shutdown().await.unwrap();
}
