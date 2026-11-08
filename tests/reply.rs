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
async fn human_reply_resumes_recovery_but_model_reply_does_not() {
    for human_reply in [true, false] {
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
        let caller = (!human_reply).then(|| Origin {
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
            if human_reply { "accepted" } else { "held" }
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
            if human_reply { "human" } else { "model" }
        );
        let state = store
            .database
            .call(|db| {
                Ok(
                    db.query_row("SELECT state FROM turn WHERE id='sender-turn'", [], |row| {
                        row.get::<_, String>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(
            state,
            if human_reply {
                "preparing"
            } else {
                "recovery_wait"
            }
        );
        store.database.shutdown().await.unwrap();
    }
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
        assert!(details.get("awaiting_reply").is_none());
        assert_eq!(details["awaiting_replies"].as_array().unwrap().len(), 1);
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
                request(Command::ThreadGet(ThreadTarget {
                    thread_id: Some("recipient".into()),
                })),
                None,
            )
            .await
            .unwrap()
            .result;
        assert!(details.get("awaiting_reply").is_none());
        assert!(details["awaiting_replies"].as_array().unwrap().is_empty());
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
async fn models_can_request_and_reply_across_threads_including_their_own() {
    let (_root, store) = fixture().await;
    let origin = Origin {
        instance_id: "fixture".into(),
        thread_id: "sender".into(),
        run_id: "run".into(),
        turn_id: "turn".into(),
        execution_id: "shell".into(),
        token: "synthetic".into(),
    };
    let mut self_send = match send(true) {
        Command::MessageSend(args) => args,
        _ => unreachable!(),
    };
    self_send.thread_id = Some("sender".into());
    let accepted = store
        .execute(
            request(Command::MessageSend(self_send)),
            Some(origin.clone()),
        )
        .await
        .unwrap()
        .result;
    assert_eq!(accepted["expect_reply"], true);
    let message_id = accepted["message_id"].as_str().unwrap().to_owned();
    let human_ask = store
        .execute(
            request(Command::MessageAsk(MessageAskArgs {
                thread_id: None,
                text: "Choose A or B".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap()
        .result;
    let details = store
        .execute(
            request(Command::ThreadGet(ThreadTarget {
                thread_id: Some("sender".into()),
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(details["awaiting_replies"].as_array().unwrap().len(), 2);
    for old in [
        "awaiting_reply",
        "awaiting_human",
        "awaiting_thread_replies",
    ] {
        assert!(details.get(old).is_none());
    }
    let duties = details["awaiting_replies"].as_array().unwrap();
    assert_eq!(
        duties
            .iter()
            .find(|d| d["recipient_kind"] == "human")
            .unwrap()["message_id"],
        human_ask["message_id"]
    );
    assert_eq!(
        duties
            .iter()
            .find(|d| d["recipient_kind"] == "thread")
            .unwrap()["message_id"],
        message_id
    );
    let not_applied = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: message_id.clone(),
                text: "self".into(),
                sender_label: None,
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(not_applied.code, ErrorCode::InvalidState);
    store.database.call({ let id = message_id.clone(); move |db| { db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&id])?; Ok(()) } }).await.unwrap();
    let reply = store
        .execute(
            request(Command::MessageReply(MessageReplyArgs {
                message_id: message_id.clone(),
                text: "self reply".into(),
                sender_label: None,
            })),
            Some(origin.clone()),
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
        .unwrap();
    assert_eq!(cross_thread.result["reply_to"], foreign_id);
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
        db.execute("UPDATE turn SET state='running' WHERE id=?1",[recovering])?;
        Ok(())
    }).await.unwrap();
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
                assert_eq!(event,("context_changed".into(),None));
                assert_eq!(db.query_row("SELECT count(*) FROM run WHERE thread_id='sender'",[],|r|r.get::<_,i64>(0))?,0,"an idle sender must not start");
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
