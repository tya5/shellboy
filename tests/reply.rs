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
    if expected {
        return Command::MessageAsk(MessageAskArgs {
            thread_id: Some("recipient".into()),
            text: "synthetic request".into(),
            deliver: Deliver::Next,
            to: None,
            human: false,
            sender_label: None,
        });
    }
    Command::MessageSend(MessageSendArgs {
        max_model_requests: None,
        resume_recovery: false,

        human: false,
        sender_label: None,
        thread_id: Some("recipient".into()),
        text: "synthetic request".into(),
        deliver: Deliver::Next,
    })
}
#[tokio::test]
async fn named_ask_leaves_only_its_recipient_list_after_reply() {
    let (_root, store) = fixture().await;
    let origin = Origin {
        instance_id: "fixture".into(),
        thread_id: "sender".into(),
        run_id: "run".into(),
        turn_id: "turn".into(),
        execution_id: "shell".into(),
        token: "synthetic".into(),
    };
    let ask = |to: Option<&str>| {
        Command::MessageAsk(MessageAskArgs {
            human: false,
            sender_label: None,
            thread_id: None,
            to: to.map(str::to_owned),
            deliver: Deliver::Next,
            text: "Choose A or B".into(),
        })
    };
    let list = |name: &str| {
        request(Command::ThreadList(ThreadListArgs {
            awaiting: Some(name.into()),
            ..Default::default()
        }))
    };
    let named_request = request(ask(Some("claude")));
    let named = store
        .execute(named_request.clone(), Some(origin.clone()))
        .await
        .unwrap();
    crate::operation_result_schema::assert_result("message.ask", &named.result);
    let named_acceptance: MessageAccepted = serde_json::from_value(named.result.clone()).unwrap();
    assert!(named_acceptance.expect_reply && named_acceptance.to_human);
    assert_eq!(named_acceptance.thread_id, "sender");
    assert_eq!(named_acceptance.assignment, Assignment::Pending);
    assert_eq!(named_acceptance.run_id, None);
    assert_eq!(named_acceptance.turn_id, None);
    crate::saved_acceptance::assert_saved_acceptance(
        &store,
        named_request.operation_id,
        &named_acceptance,
    )
    .await;
    assert!(
        named
            .changed_threads
            .iter()
            .any(|thread| thread == "sender")
    );
    let pending = store.execute(list("claude"), None).await.unwrap().result;
    assert_eq!(pending["items"].as_array().unwrap().len(), 1);
    assert_eq!(pending["items"][0]["thread_id"], "sender");
    assert_eq!(
        store.execute(list("human"), None).await.unwrap().result["items"],
        serde_json::json!([])
    );
    store
        .execute(request(ask(None)), Some(origin.clone()))
        .await
        .unwrap();
    let reply = request(Command::MessageReply(MessageReplyArgs {
        message_id: named.result["message_id"].as_str().unwrap().into(),
        text: "Choose A".into(),
        sender_label: Some("not the addressee".into()),
    }));
    let answer = store.execute(reply.clone(), None).await.unwrap().result;
    crate::operation_result_schema::assert_result("message.reply", &answer);
    assert_eq!(answer["message_id"], answer["delivery"]["message_id"]);
    let delivery: MessageAccepted = serde_json::from_value(answer["delivery"].clone()).unwrap();
    crate::saved_acceptance::assert_saved_acceptance(&store, reply.operation_id.clone(), &delivery)
        .await;
    assert_eq!(answer["reply_to"], named.result["message_id"]);
    assert_eq!(store.execute(reply, None).await.unwrap().result, answer);
    let history = |id: &str| {
        request(
            serde_json::from_value(serde_json::json!({
                "command":"history.read", "args":{"node_id":id}
            }))
            .unwrap(),
        )
    };
    let answer_node = store
        .execute(history(answer["message_id"].as_str().unwrap()), None)
        .await
        .unwrap()
        .result;
    assert_eq!(
        answer_node["provenance"]["sender"],
        serde_json::json!({"kind":"external_cli","label":"claude"})
    );
    let exact = format!("clau\nde\u{2028}{}", "x".repeat(80));
    let special_ask = store
        .execute(request(ask(Some(&exact))), Some(origin.clone()))
        .await
        .unwrap()
        .result;
    let ask_id = special_ask["message_id"].as_str().unwrap();
    let duty = store
        .execute(
            request(Command::MessageList(MessageListArgs {
                to: Some(exact),
                owed_by_thread_id: None,
                asked_by_thread_id: None,
                limit: DEFAULT_LIST_LIMIT,
                cursor: None,
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(duty["items"][0]["message_id"], ask_id);
    let answer = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: ask_id.into(),
                text: "answer".into(),
                sender_label: None,
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    let answer_node = store
        .execute(history(answer["message_id"].as_str().unwrap()), None)
        .await
        .unwrap()
        .result;
    assert_eq!(
        answer_node["provenance"]["sender"],
        serde_json::json!({"kind":"external_cli","label":format!("claude{}", "x".repeat(58))})
    );
    assert_eq!(
        store.execute(list("claude"), None).await.unwrap().result["items"],
        serde_json::json!([])
    );
    assert_eq!(
        store.execute(list("human"), None).await.unwrap().result["items"][0]["thread_id"],
        "sender"
    );
    let duty = store
        .execute(
            request(Command::MessageAsk(MessageAskArgs {
                human: false,
                sender_label: Some("Release assistant".into()),
                thread_id: None,
                to: Some("human".into()),
                deliver: Deliver::Next,
                text: "Should the release preview open on Overview or Timeline?".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap()
        .result;
    let ask_id = duty["message_id"].as_str().unwrap();
    let ask_node = store.execute(history(ask_id), None).await.unwrap().result;
    assert_eq!(ask_node["provenance"]["kind"], "assistant");
    assert_eq!(
        ask_node["provenance"]["sender"],
        serde_json::json!({
            "kind":"model", "label":"Release assistant", "thread_id":"sender",
            "instance_id":"fixture", "run_id":"run", "turn_id":"turn", "execution_id":"shell"
        })
    );
    assert!(ask_node["provenance"].get("client_operation_id").is_none());
    let answer = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: duty["message_id"].as_str().unwrap().into(),
                text: "answer".into(),
                sender_label: None,
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    let answer_node = store
        .execute(history(answer["message_id"].as_str().unwrap()), None)
        .await
        .unwrap()
        .result;
    assert_eq!(
        answer_node["provenance"]["sender"],
        serde_json::json!({"kind":"human"})
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn expected_reply_cap_is_16_and_replay_is_not_a_new_duty() {
    let (_root, store) = fixture().await;
    let first = request(send(true));
    let accepted = store.execute(first.clone(), None).await.unwrap().result;
    assert_eq!(accepted["expect_reply"], true);
    for _ in 1..16 {
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
        ErrorCode::ReplyLimit
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
    crate::operation_result_schema::assert_result("message.reply", &result);
    let replied: MessageReplied = serde_json::from_value(result.clone()).unwrap();
    assert!(replied.delivery.is_none());
    assert!(result.get("delivery").is_none());
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
async fn originless_thread_duty_reply_resumes_recovery_but_model_reply_does_not() {
    for (originless, backlog) in [(true, 0), (true, 129), (false, 0)] {
        let (_root, store) = fixture().await;
        let model = Origin {
            instance_id: "fixture".into(),
            thread_id: "sender".into(),
            run_id: "sender-origin-run".into(),
            turn_id: "sender-origin-turn".into(),
            execution_id: "sender-shell".into(),
            token: "synthetic".into(),
        };
        let accepted = store
            .execute(request(send(true)), Some(model.clone()))
            .await
            .unwrap()
            .result;
        let id = accepted["message_id"].as_str().unwrap().to_owned();
        let message = id.clone();
        store.database.call(move |db| {
            db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&message])?;
            db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('sender-run','sender','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES('sender-turn','sender','sender-run','recovery_wait','now','{}');")?;
            Ok(())
        }).await.unwrap();
        for _ in 0..backlog {
            let Command::MessageSend(input) = send(false) else {
                unreachable!();
            };
            let held = store
                .execute(
                    request(Command::MessageSend(MessageSendArgs {
                        thread_id: Some("sender".into()),
                        ..input
                    })),
                    None,
                )
                .await
                .unwrap()
                .result;
            assert_eq!(held["delivery_status"], "held");
        }
        let caller = (!originless).then(|| Origin {
            thread_id: "recipient".into(),
            ..model
        });
        let result = store
            .execute(
                request(Command::MessageReply(MessageReplyArgs {
                    message_id: id,
                    text: "synthetic answer".into(),
                    sender_label: None,
                })),
                caller,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(
            result["delivery"]["delivery_status"],
            if originless { "accepted" } else { "held" }
        );
        let reply_message = store
            .execute(
                request(Command::MessageGet(MessageTarget {
                    message_id: result["message_id"].as_str().unwrap().to_owned(),
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(
            reply_message["sender"]["kind"],
            if originless { "external_cli" } else { "model" }
        );
        let (state, remaining) = store
            .database
            .call(|db| {
                let state: String = db.query_row(
                    "SELECT state FROM turn WHERE id='sender-turn'", [], |row| row.get(0),
                )?;
                let remaining: u32 = db.query_row(
                    "SELECT count(*) FROM receipt WHERE turn_id='sender-turn' AND state='held' AND hold_reason='recovery_wait'",
                    [], |row| row.get(0),
                )?;
                Ok((state, remaining))
            })
            .await
            .unwrap();
        assert_eq!(
            state,
            if originless && backlog == 0 {
                "preparing"
            } else {
                "recovery_wait"
            }
        );
        assert_eq!(remaining, u32::from(backlog > 0 || !originless));
        store.database.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn delivered_cross_thread_ask_can_be_withdrawn_only_by_its_sender_or_owner() {
    for owner_override in [false, true] {
        let (_root, store) = fixture().await;
        let sender = Origin {
            instance_id: "fixture".into(),
            thread_id: "sender".into(),
            run_id: "sender-run".into(),
            turn_id: "sender-turn".into(),
            execution_id: "sender-shell".into(),
            token: "synthetic".into(),
        };
        let recipient = Origin {
            thread_id: "recipient".into(),
            ..sender.clone()
        };
        let accepted = store
            .execute(request(send(true)), Some(sender.clone()))
            .await
            .unwrap();
        let id = accepted.result["message_id"].as_str().unwrap().to_owned();
        let node = id.clone();
        store.database.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&node])?;
            tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('recipient',0,?1)", [&node])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let cancel = || {
            request(Command::MessageCancel(MessageCancelArgs {
                message_ids: vec![id.clone()],
            }))
        };
        let rejected = store
            .execute(cancel(), Some(recipient.clone()))
            .await
            .unwrap();
        assert_eq!(rejected.result["items"][0]["outcome"], "forbidden");
        let get = || {
            request(Command::MessageGet(MessageTarget {
                message_id: id.clone(),
            }))
        };
        assert_eq!(
            store.execute(get(), None).await.unwrap().result["reply"]["state"],
            "pending"
        );
        let cancelled = store
            .execute(cancel(), (!owner_override).then_some(sender))
            .await
            .unwrap();
        assert_eq!(cancelled.result["items"][0]["outcome"], "reply_cancelled");
        assert!(cancelled.quiet_wake_threads.is_empty());
        // The recipient gets a saved notice; only the sender gets the existing outcome wake.
        assert_eq!(cancelled.quiet_wake_threads, ["sender"]);
        for thread in ["sender", "recipient"] {
            assert!(cancelled.changed_threads.iter().any(|id| id == thread));
        }
        let saved = store.execute(get(), None).await.unwrap().result;
        assert_eq!(saved["delivery_status"], "applied");
        assert_eq!(saved["reply"]["state"], "cancelled");
        assert_eq!(
            saved["reply"]["cancelled_by"]["kind"],
            if owner_override { "human" } else { "model" }
        );
        let late = store
            .execute(
                request(Command::MessageReply(MessageReplyArgs {
                    message_id: id.clone(),
                    text: "late answer".into(),
                    sender_label: None,
                })),
                Some(recipient),
            )
            .await
            .unwrap_err();
        assert_eq!(late.code, ErrorCode::InvalidState);
        assert_eq!(
            late.details.unwrap(),
            serde_json::json!({"reply_state":"cancelled","reason":"withdrawn"})
        );
        store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row(
                        "SELECT node_id FROM thread_path WHERE thread_id='recipient' AND ordinal=0",
                        [],
                        |row| row.get::<_, String>(0)
                    )?,
                    id
                );
                assert_eq!(
                    db.query_row(
                        "SELECT target_thread_id FROM event WHERE id=?1 AND target_run_id IS NULL",
                        [format!("reply-cancelled-{id}")],
                        |row| row.get::<_, String>(0)
                    )?,
                    "recipient"
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM receipt", [], |row| row
                        .get::<_, i64>(0))?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
        store.database.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_releases_duty_and_lists_no_longer_awaiting() {
    for admitted in [false, true] {
        let (_root, store) = fixture().await;
        let ask = request(send(true));
        let accepted = store.execute(ask.clone(), None).await.unwrap().result;
        crate::operation_result_schema::assert_result("message.ask", &accepted);
        let thread_acceptance: MessageAccepted = serde_json::from_value(accepted.clone()).unwrap();
        assert!(thread_acceptance.expect_reply && !thread_acceptance.to_human);
        assert_eq!(thread_acceptance.thread_id, "recipient");
        assert_eq!(thread_acceptance.assignment, Assignment::NewRun);
        assert!(thread_acceptance.run_id.is_some() && thread_acceptance.turn_id.is_some());
        crate::saved_acceptance::assert_saved_acceptance(
            &store,
            ask.operation_id,
            &thread_acceptance,
        )
        .await;
        let id = thread_acceptance.message_id;
        let details = store
            .execute(
                request(Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
                    thread_id: Some("recipient".into()),
                    display: false,
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert!(details.get("awaiting_reply").is_none());
        assert_eq!(
            details["pending_replies"]["owed_by"],
            serde_json::json!({"count":1,"exact":true})
        );
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
        assert_eq!(
            details["reply"]["cancelled_by"],
            serde_json::json!({"kind":"human"})
        );
        assert_eq!(details["reply"]["cancel_reason"], "sender");
        assert!(details["reply"].get("by").is_none());
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
                request(Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
                    thread_id: Some("recipient".into()),
                    display: false,
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert!(details.get("awaiting_reply").is_none());
        assert_eq!(details["pending_replies"]["owed_by"]["count"], 0);
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
async fn models_can_request_and_reply_across_threads() {
    let (_root, store) = fixture().await;
    let origin = Origin {
        instance_id: "fixture".into(),
        thread_id: "sender".into(),
        run_id: "run".into(),
        turn_id: "turn".into(),
        execution_id: "shell".into(),
        token: "synthetic".into(),
    };
    let recipient = Origin {
        thread_id: "recipient".into(),
        ..origin.clone()
    };
    let accepted = store
        .execute(request(send(true)), Some(origin.clone()))
        .await
        .unwrap()
        .result;
    assert_eq!(accepted["expect_reply"], true);
    let message_id = accepted["message_id"].as_str().unwrap().to_owned();
    let human_ask = store
        .execute(
            request(Command::MessageAsk(MessageAskArgs {
                human: false,
                sender_label: None,
                thread_id: None,
                to: None,
                deliver: Deliver::Next,
                text: "Choose A or B".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap()
        .result;
    let Command::MessageAsk(mut external) = send(true) else {
        unreachable!();
    };
    external.sender_label = Some("external-tool".into());
    let external = store
        .execute(request(Command::MessageAsk(external)), None)
        .await
        .unwrap()
        .result;
    let external_id = external["message_id"].as_str().unwrap();
    for (thread, direction) in [("recipient", "owed_by"), ("sender", "asked_by")] {
        let details = store
            .execute(
                request(Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
                    thread_id: Some(thread.into()),
                    display: false,
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert!(details.get("awaiting_replies").is_none());
        assert_eq!(details["pending_replies"][direction]["count"], 2);
    }
    for (owed_by, asked_by, to, expected) in [
        (
            Some("recipient"),
            None,
            None,
            vec![message_id.as_str(), external_id],
        ),
        (
            None,
            Some("sender"),
            None,
            vec![
                message_id.as_str(),
                human_ask["message_id"].as_str().unwrap(),
            ],
        ),
        (
            None,
            None,
            Some("human"),
            vec![human_ask["message_id"].as_str().unwrap()],
        ),
    ] {
        let page = store
            .execute(
                request(Command::MessageList(MessageListArgs {
                    owed_by_thread_id: owed_by.map(str::to_owned),
                    asked_by_thread_id: asked_by.map(str::to_owned),
                    to: to.map(str::to_owned),
                    limit: 10,
                    cursor: None,
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        let ids: Vec<_> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["message_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), expected.len());
        assert!(expected.iter().all(|id| ids.contains(id)));
        for item in page["items"].as_array().unwrap() {
            if item["message_id"] == external_id {
                assert_eq!(item["asked_by"], serde_json::Value::Null);
                assert_eq!(
                    item["sender"],
                    serde_json::json!({"kind":"external_cli","label":"external-tool"})
                );
            } else {
                assert_eq!(item["asked_by"], "sender");
                assert_eq!(
                    item["sender"],
                    serde_json::json!({
                        "kind":"model", "instance_id":"fixture", "thread_id":"sender",
                        "run_id":"run", "turn_id":"turn", "execution_id":"shell"
                    })
                );
            }
        }
    }
    let not_applied = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: message_id.clone(),
                text: "too early".into(),
                sender_label: None,
            })),
            Some(recipient.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(not_applied.code, ErrorCode::InvalidState);
    store.database.call({ let id = message_id.clone(); move |db| { db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&id])?; Ok(()) } }).await.unwrap();
    for id in [
        message_id.as_str(),
        human_ask["message_id"].as_str().unwrap(),
    ] {
        let error = store
            .execute(
                request(Command::MessageReply(MessageReplyArgs {
                    message_id: id.into(),
                    text: "not my answer".into(),
                    sender_label: None,
                })),
                Some(origin.clone()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        let saved = store
            .execute(
                request(Command::MessageGet(MessageTarget {
                    message_id: id.into(),
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert_eq!(saved["reply"]["state"], "pending");
    }
    let reply = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: message_id.clone(),
                text: "recipient reply".into(),
                sender_label: None,
            })),
            Some(recipient),
        )
        .await
        .unwrap();
    assert_eq!(reply.result["reply_to"], message_id);
    let foreign = store
        .execute(request(send(true)), None)
        .await
        .unwrap()
        .result;
    let foreign_id = foreign["message_id"].as_str().unwrap().to_owned();
    store.database.call({ let id = foreign_id.clone(); move |db| { db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&id])?; Ok(()) } }).await.unwrap();
    let cross_thread = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: foreign_id.clone(),
                text: "cross-thread".into(),
                sender_label: None,
            })),
            Some(Origin {
                thread_id: "sender".into(),
                ..origin
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(cross_thread.code, ErrorCode::TargetUnavailable);
    assert_eq!(
        cross_thread.details.unwrap()["pending_duty_ids"],
        serde_json::json!([])
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_wait_keeps_pending_and_recovered_reply_wakes_completed_sender_once() {
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
    let recovering = accepted["turn_id"].as_str().unwrap().to_owned();
    let recovery_id = id.clone();
    store.database.call(move|db| {
        db.execute("UPDATE turn SET state='recovery_wait' WHERE id=?1",[&recovering])?;
        assert_eq!(db.query_row("SELECT json_extract(result_json,'$.reply.state') FROM receipt WHERE message_node_id=?1",[recovery_id],|r|r.get::<_,String>(0))?,"pending");
        Ok(())
    }).await.unwrap();
    store
        .execute(
            request(Command::RunRetry(RunTarget {
                run_id: accepted["run_id"].as_str().unwrap().into(),
            })),
            None,
        )
        .await
        .unwrap();
    // No live parent Run is required for a reply: delivery starts ordinary new work.
    store
        .database
        .call(|db| {
            db.execute(
                "UPDATE turn SET state='completed',ended_at='now' WHERE run_id='sender-run'",
                [],
            )?;
            db.execute(
                "UPDATE run SET state='completed',ended_at='now' WHERE id='sender-run'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
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
    assert_eq!(result["delivery"]["assignment"], "new_run");
    assert_ne!(result["delivery"]["run_id"], "sender-run");
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

#[tokio::test]
async fn deletion_terminalizes_replies_before_gc_and_recipient_wins() {
    for deleted in ["sender", "recipient", "both"] {
        let (_root, store) = fixture().await;
        let a = store
            .execute(request(send(true)), None)
            .await
            .unwrap()
            .result;
        let message = a["message_id"].as_str().unwrap().to_owned();
        let id = message.clone();
        store.database.call(move|db| {
            db.execute("UPDATE receipt SET sender_json=?2,state='applied',path_admission='admitted' WHERE message_node_id=?1",rusqlite::params![id,serde_json::json!({"kind":"model","instance_id":"i","thread_id":"sender","run_id":"r","turn_id":"u","execution_id":"s"}).to_string()])?;
            db.execute_batch("UPDATE run SET state='stopped',ended_at='now'; UPDATE turn SET state='stopped',ended_at='now';")?;
            if deleted=="both" { db.execute("UPDATE thread SET parent_thread_id='sender' WHERE id='recipient'",[])?; }
            Ok(())
        }).await.unwrap();
        let thread = if deleted == "recipient" {
            "recipient"
        } else {
            "sender"
        };
        store
            .execute(
                request(Command::ThreadDelete(ThreadDeleteArgs {
                    thread_id: thread.into(),
                    descendants: deleted == "both",
                    preview_marker: None,
                })),
                None,
            )
            .await
            .unwrap();
        let m = message.clone();
        store.database.call(move|db| {
            let raw:String=db.query_row("SELECT result_json FROM receipt WHERE message_node_id=?1",[&m],|r|r.get(0))?;
            let value:serde_json::Value=serde_json::from_str(&raw).unwrap();
            assert_eq!(value["reply"]["state"],if deleted=="sender"{"pending"}else{"recipient_deleted"});
            if deleted=="sender" {
                assert_eq!(db.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='reply_cancelled'",[],|r|r.get::<_,i64>(0))?,0,"sender cleanup is deferred");
            }
            if deleted=="recipient" {
                let event:(String,Option<String>)=db.query_row("SELECT kind,target_run_id FROM event WHERE json_extract(payload_ref_json,'$.kind')='reply_outcome'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
                assert_eq!(event.0,"reply_outcome");
                assert!(event.1.is_some());
                assert_eq!(db.query_row("SELECT count(*) FROM run WHERE thread_id='sender' AND state='live'",[],|r|r.get::<_,i64>(0))?,1,"ordinary outcome delivery activates an idle sender");
            }
            assert_eq!(db.query_row("SELECT count(*) FROM gc_job WHERE state='pending'",[],|r|r.get::<_,i64>(0))?,1);
            Ok(())
        }).await.unwrap();
        let attachments = Arc::new(shellboy::attachments::AttachmentService::new(
            store.database.clone(),
            &store.config,
            _root.path().to_owned(),
            "fixture".into(),
        ));
        let gc = shellboy::gc::GcService::new(
            store.database.clone(),
            store.config.clone(),
            _root.path().to_owned(),
            attachments,
        );
        let job = gc.start_history().await.unwrap().unwrap();
        let mut complete = false;
        for _ in 0..64 {
            if gc.history_step(&job).await.unwrap().complete {
                complete = true;
                break;
            }
        }
        assert!(
            complete,
            "delete-scheduled GC must finish its original horizon"
        );
        store
            .database
            .call(move |db| {
                if deleted == "sender" {
                    let raw: String = db.query_row("SELECT result_json FROM receipt WHERE message_node_id=?1", [&message], |r|r.get(0))?;
                    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
                    assert_eq!(value["reply"]["state"], "cancelled");
                    assert_eq!(value["reply"]["cancel_reason"], "sender_deleted");
                    assert_eq!(value["reply"]["cancelled_by"]["thread_id"], "sender");
                    assert_eq!(db.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='reply_cancelled'",[],|r|r.get::<_,i64>(0))?,1);
                }
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM receipt WHERE message_node_id=?1",
                        [&message],
                        |r| r.get::<_, i64>(0)
                    )?,
                    if deleted == "both" { 0 } else { 1 }
                );
                Ok(())
            })
            .await
            .unwrap();
        store.database.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn replying_after_sender_deletion_before_gc_cancels_instead_of_replying() {
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
        .execute(request(send(true)), Some(sender))
        .await
        .unwrap()
        .result;
    let message = accepted["message_id"].as_str().unwrap().to_owned();
    let id = message.clone();
    store.database.call(move |db| {
        db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1",[id])?;
        db.execute_batch("UPDATE run SET state='stopped',ended_at='now'; UPDATE turn SET state='stopped',ended_at='now';")?;
        Ok(())
    }).await.unwrap();
    store
        .execute(
            request(Command::ThreadDelete(ThreadDeleteArgs {
                thread_id: "sender".into(),
                descendants: false,
                preview_marker: None,
            })),
            None,
        )
        .await
        .unwrap();
    let before = store
        .execute(
            request(Command::MessageGet(MessageTarget {
                message_id: message.clone(),
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(before["reply"]["state"], "pending");
    let reply = request(Command::MessageReply(MessageReplyArgs {
        message_id: message.clone(),
        text: "must not be saved".into(),
        sender_label: None,
    }));
    let error = store.execute(reply.clone(), None).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(error.details.as_ref().unwrap()["reason"], "sender_deleted");
    assert_eq!(store.execute(reply, None).await.unwrap_err(), error);
    let details = store
        .execute(
            request(Command::MessageGet(MessageTarget {
                message_id: message.clone(),
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(details["reply"]["state"], "cancelled");
    assert_eq!(details["reply"]["cancel_reason"], "sender_deleted");
    assert_eq!(details["reply"]["cancelled_by"]["thread_id"], "sender");
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM event WHERE id=?1",
                    [format!("reply-cancelled-{message}")],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM node WHERE payload_json LIKE '%must not be saved%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM gc_job WHERE state='pending'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1,
                "GC has not run"
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_held_queue_progresses_through_send_and_retry_without_rejection() {
    let (_root, store) = fixture().await;
    let initial = store
        .execute(request(send(false)), None)
        .await
        .unwrap()
        .result;
    let old_run = initial["run_id"].as_str().unwrap().to_owned();
    let old_turn = initial["turn_id"].as_str().unwrap().to_owned();
    store
        .database
        .call(move |db| {
            db.execute("UPDATE run SET state='stopping' WHERE id=?1", [&old_run])?;
            Ok(())
        })
        .await
        .unwrap();
    let mut ids = Vec::new();
    for _ in 0..129 {
        let result = store
            .execute(request(send(false)), None)
            .await
            .unwrap()
            .result;
        assert_eq!(result["delivery_status"], "held");
        ids.push(result["message_id"].as_str().unwrap().to_owned());
    }
    store
        .database
        .call(move |db| {
            db.execute("UPDATE turn SET state='stopped' WHERE id=?1", [&old_turn])?;
            db.execute(
                "UPDATE run SET state='stopped' WHERE thread_id='recipient'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let resume = request(send(false));
    let result = store.execute(resume.clone(), None).await.unwrap().result;
    assert_eq!(result["delivery_status"], "held");
    let run = result["run_id"].as_str().unwrap().to_owned();
    assert_eq!(store.execute(resume, None).await.unwrap().result, result);
    let status = store
        .execute(
            request(Command::RunGet(RunTarget {
                run_id: run.clone(),
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    let hint: serde_json::Value =
        serde_json::from_str(status["recovery_hint"].as_str().unwrap()).unwrap();
    assert_eq!(hint["remaining_held"], 3);
    let retry = request(Command::RunRetry(RunTarget { run_id: run }));
    let retried = store.execute(retry.clone(), None).await.unwrap().result;
    assert!(retried["recovery_hint"].is_null());
    assert_eq!(store.execute(retry, None).await.unwrap().result, retried);
    let (selected, held) = store
        .database
        .call(|db| {
            let selected = db
                .prepare(
                    "SELECT node_id FROM thread_path WHERE thread_id='recipient' ORDER BY ordinal",
                )?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let held: i64 = db.query_row(
                "SELECT count(*) FROM receipt WHERE target_thread_id='recipient' AND state='held'",
                [],
                |r| r.get(0),
            )?;
            Ok((selected, held))
        })
        .await
        .unwrap();
    // Initial accepted input is recovered before the 129 stopping-held inputs.
    assert_eq!(&selected[1..=ids.len()], ids);
    assert_eq!(
        selected.last().unwrap(),
        result["message_id"].as_str().unwrap()
    );
    assert_eq!(held, 0);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_reply_target_lists_only_calling_threads_pending_duties() {
    let (_root, store) = fixture().await;
    let recipient = store
        .execute(request(send(true)), None)
        .await
        .unwrap()
        .result;
    let recipient_id = recipient["message_id"].as_str().unwrap().to_owned();
    let unrelated = store
        .execute(
            request(Command::MessageAsk(MessageAskArgs {
                thread_id: Some("sender".into()),
                ..match send(true) {
                    Command::MessageAsk(args) => args,
                    _ => unreachable!(),
                }
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    let unrelated_id = unrelated["message_id"].as_str().unwrap().to_owned();
    let caller = Origin {
        instance_id: "fixture".into(),
        thread_id: "recipient".into(),
        run_id: "caller-run".into(),
        turn_id: "caller-turn".into(),
        execution_id: "caller-shell".into(),
        token: "synthetic-origin".into(),
    };
    store
        .execute(
            request(Command::MessageAsk(MessageAskArgs {
                human: false,
                sender_label: None,
                thread_id: None,
                to: None,
                deliver: Deliver::Next,
                text: "question I asked, not a duty I owe".into(),
            })),
            Some(caller.clone()),
        )
        .await
        .unwrap();
    let resolve_error = store
        .execute(
            request(Command::IdResolve(IdResolveArgs {
                kind: IdKind::Message,
                prefix: "deadbeef".into(),
                include_pending_duties: true,
            })),
            Some(caller.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(resolve_error.code, ErrorCode::NotFound);
    assert_eq!(
        resolve_error.details.as_ref().unwrap()["pending_duty_ids"],
        serde_json::json!([recipient_id])
    );
    assert_ne!(
        resolve_error.details.as_ref().unwrap()["pending_duty_ids"][0],
        unrelated_id
    );

    let plain_resolve_error = store
        .execute(
            request(Command::IdResolve(IdResolveArgs {
                kind: IdKind::Message,
                prefix: "deadbeef".into(),
                include_pending_duties: false,
            })),
            Some(caller.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(plain_resolve_error.code, ErrorCode::NotFound);
    assert!(
        plain_resolve_error
            .details
            .as_ref()
            .unwrap()
            .get("pending_duty_ids")
            .is_none()
    );

    let error = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: "missing-message".into(),
                text: "synthetic reply".into(),
                sender_label: None,
            })),
            Some(caller),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TargetUnavailable);
    assert_eq!(
        error.details.as_ref().unwrap()["pending_duty_ids"],
        serde_json::json!([recipient_id])
    );
    assert_ne!(
        error.details.as_ref().unwrap()["pending_duty_ids"][0],
        unrelated_id
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn id_resolve_is_a_receipt_free_observation() {
    let (_root, store) = fixture().await;
    let resolve_request = Request::new(
        "fixture:id-resolve-observation",
        Command::IdResolve(IdResolveArgs {
            kind: IdKind::Thread,
            prefix: "recipien".into(),
            include_pending_duties: false,
        }),
    );
    assert!(resolve_request.command.is_read_only());
    let operation_id = resolve_request.operation_id.clone();
    let direct_prefix = store
        .execute(
            request(Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
                thread_id: Some("recipien".into()),
                display: false,
            })),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        direct_prefix.code,
        ErrorCode::ThreadNotFound,
        "wire target IDs remain exact"
    );
    let response = store.execute(resolve_request, None).await.unwrap();
    assert_eq!(response.result["id"], "recipient");
    let receipt_count = store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM operation_receipt WHERE operation_id=?1",
                [&operation_id],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(receipt_count, 0);
    store.database.shutdown().await.unwrap();
}
