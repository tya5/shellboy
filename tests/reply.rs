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
    // The rejected operation is durably replayable; retry with a new operation.
    let reply = request(reply.command.clone());
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
    assert_eq!(late.details.as_ref().unwrap()["reply_state"], "replied");
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_releases_duty_and_lists_no_longer_awaiting() {
    for admitted in [false, true] {
        let (_root, store) = fixture().await;
        let accepted = store
            .execute(request(send(true)), None)
            .await
            .unwrap()
            .result;
        let id = accepted["message_id"].as_str().unwrap().to_owned();
        let details = store
            .execute(
                request(Command::ThreadGet(ThreadTarget {
                    thread_id: Some("recipient".into()),
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(details["awaiting_reply"].as_array().unwrap().len(), 1);
        if admitted {
            let node = id.clone();
            store.database.call(move |db| { db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1",[node])?;Ok(()) }).await.unwrap();
        }
        let cancel = store
            .execute(
                request(Command::MessageCancel(MessageCancelArgs {
                    message_ids: vec![id.clone()],
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(
            cancel["items"][0]["outcome"],
            if admitted {
                "reply_cancelled"
            } else {
                "cancelled"
            }
        );
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
        assert_eq!(details["reply"]["state"], "cancelled");
        store.database.call(move |db| {
            assert_eq!(db.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='reply_cancelled'", [], |r| r.get::<_, i64>(0))?, if admitted { 1 } else { 0 });
            assert_eq!(db.query_row("SELECT count(*) FROM thread_path", [], |r| r.get::<_, i64>(0))?, 0);
            Ok(())
        }).await.unwrap();
        assert_eq!(
            details["delivery_status"],
            if admitted { "applied" } else { "cancelled" }
        );
        let details = store
            .execute(
                request(Command::ThreadGet(ThreadTarget {
                    thread_id: Some("recipient".into()),
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert!(details["awaiting_reply"].as_array().unwrap().is_empty());
        let error = store
            .execute(
                request(Command::MessageReply(MessageReplyArgs {
                    message_id: id,
                    text: "too late".into(),
                    sender_label: None,
                })),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.details.as_ref().unwrap()["reply_state"], "cancelled");
        store.database.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn models_cannot_request_their_own_reply_or_reply_for_another_thread() {
    let (_root, store) = fixture().await;
    let origin = Origin {
        instance_id: "fixture".into(),
        thread_id: "recipient".into(),
        run_id: "run".into(),
        turn_id: "turn".into(),
        execution_id: "shell".into(),
        token: "synthetic".into(),
    };
    let error = store
        .execute(request(send(true)), Some(origin.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    let accepted = store
        .execute(request(send(true)), None)
        .await
        .unwrap()
        .result;
    let id = accepted["message_id"].as_str().unwrap().to_owned();
    let wrong = Origin {
        thread_id: "sender".into(),
        ..origin
    };
    let error = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: id,
                text: "unauthorized".into(),
                sender_label: None,
            })),
            Some(wrong),
        )
        .await
        .unwrap_err();
    assert_eq!(error.details.as_ref().unwrap()["reason"], "not_recipient");
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn reply_to_model_is_delivered_once_and_preserves_correlation() {
    let (_root, store) = fixture().await;
    let sender = Origin {
        instance_id: "fixture".into(),
        thread_id: "sender".into(),
        run_id: "sender-run".into(),
        turn_id: "sender-turn".into(),
        execution_id: "sender-shell".into(),
        token: "synthetic".into(),
    };
    let accepted = store
        .execute(request(send(true)), Some(sender.clone()))
        .await
        .unwrap()
        .result;
    let id = accepted["message_id"].as_str().unwrap().to_owned();
    let applied = id.clone();
    store.database.call(move |db| {
        db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [applied])?;
        Ok(())
    }).await.unwrap();
    let recipient = Origin {
        thread_id: "recipient".into(),
        ..sender
    };
    let operation = request(Command::MessageReply(MessageReplyArgs {
        message_id: id.clone(),
        text: "synthetic model reply".into(),
        sender_label: None,
    }));
    let result = store
        .execute(operation.clone(), Some(recipient.clone()))
        .await
        .unwrap()
        .result;
    assert_eq!(result["delivery"]["thread_id"], "sender");
    assert_eq!(
        store
            .execute(operation, Some(recipient))
            .await
            .unwrap()
            .result,
        result
    );
    let reply_id = result["message_id"].as_str().unwrap().to_owned();
    let details = store
        .execute(
            request(Command::MessageGet(MessageTarget {
                message_id: reply_id,
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(details["reply_to"], id);
    assert!(details.get("answer_node_id").is_none());
    assert!(details.get("reply").is_none());
    assert_eq!(details["sender"]["kind"], "model");
    store.database.shutdown().await.unwrap();
}
