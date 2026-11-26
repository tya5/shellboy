use super::*;
use crate::{
    control::db::{DraftSendOptions, DraftSubmission, Store},
    protocol::*,
    storage::DbWorker,
};
use std::sync::Arc;

async fn store() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("pending.db"))
        .await
        .unwrap();
    let config=Config::parse("default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let store = Store::new(db, Arc::new(config), root.path().into(), "fixture".into());
    store.database.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','fixture','synthetic','0'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','0'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','0');")?;
        Ok(())
    }).await.unwrap();
    (root, store)
}
fn origin(thread: &str) -> Origin {
    Origin {
        instance_id: "fixture".into(),
        thread_id: thread.into(),
        run_id: "source-run".into(),
        turn_id: "source-turn".into(),
        execution_id: "source-shell".into(),
        token: "private-origin-token".into(),
    }
}
fn input(db: &Connection, id: &str, sender: Option<&Origin>) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'u','user','0',?2)",
        params![
            id,
            json!({"role":"user","content":[{"type":"input_text","text":id}]}).to_string()
        ],
    )?;
    db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,result_json)VALUES(?1,?1,?2,'t',?1,'r','u','accepted','0','{}')",params![id,serde_json::to_string(&sender.map(Origin::identity)).unwrap()])?;
    Ok(())
}
fn request(id: &str, ids: &[&str]) -> Request {
    Request::new(
        format!("fixture:{id}"),
        Command::MessageCancel(MessageCancelArgs {
            message_ids: ids.iter().map(|id| (*id).into()).collect(),
        }),
    )
}

async fn admit_at_real_boundary(store: &Store, nodes: Vec<String>) {
    store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let revision =
                crate::history::selection_revision(&tx, "t", crate::history::ThreadScope::Live)?;
            crate::control::message_delivery::append_admitted_tx(
                &tx,
                "t",
                revision as u64,
                &nodes,
                &chrono::Utc::now().to_rfc3339(),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn human_question_reply_cancel_and_attachment_frontier_preserve_fifo() {
    let (root, store) = store().await;
    store
        .database
        .call(|db| {
            db.execute(
                "UPDATE thread SET archived_at='2026-01-01' WHERE id='t'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let before_counts = store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM run WHERE thread_id='t'", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("SELECT count(*) FROM turn WHERE thread_id='t'", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            ))
        })
        .await
        .unwrap();

    let ask = |operation: &'static str, text: &'static str| {
        Request::new(
            format!("fixture:{operation}"),
            Command::MessageAsk(MessageAskArgs {
                human: false,
                sender_label: None,
                thread_id: None,
                to: None,
                deliver: Deliver::Next,
                text: text.into(),
            }),
        )
    };
    let first = store
        .execute(
            ask("question-first", "first synthetic question"),
            Some(origin("t")),
        )
        .await
        .unwrap();
    assert!(first.quiet_wake_threads.is_empty());
    let first_accepted: MessageAccepted = serde_json::from_value(first.result.clone()).unwrap();
    assert_eq!(first_accepted.delivery_status, DeliveryStatus::Accepted);
    assert_eq!(first_accepted.assignment, Assignment::Pending);
    let first_id = first_accepted.message_id;
    let first_before: (String, String, Option<String>, Option<String>, bool, Option<String>) = store.database.call({
        let id = first_id.clone();
        move |db| db.query_row(
            "SELECT r.state,r.path_admission,r.run_id,r.turn_id,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id='t' AND p.node_id=r.message_node_id),t.archived_at FROM receipt r JOIN thread t ON t.id=r.target_thread_id WHERE r.message_node_id=?1",
            [&id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).map_err(Into::into)
    }).await.unwrap();
    assert_eq!(
        first_before,
        (
            "applied".into(),
            "unadmitted".into(),
            None,
            None,
            false,
            None
        )
    );
    assert_eq!(
        store
            .database
            .call(|db| Ok((
                db.query_row("SELECT count(*) FROM run WHERE thread_id='t'", [], |r| r
                    .get::<_, i64>(0))?,
                db.query_row("SELECT count(*) FROM turn WHERE thread_id='t'", [], |r| r
                    .get::<_, i64>(
                    0
                ))?,
            )))
            .await
            .unwrap(),
        before_counts,
        "asking a human creates no model Run or Turn"
    );

    // An idle boundary admits the question without creating or waking a model Turn.
    admit_at_real_boundary(&store, vec![]).await;
    let first_admitted: (String, bool, i64) = store.database.call({
        let id = first_id.clone();
        move |db| Ok(db.query_row(
            "SELECT r.path_admission,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id='t' AND p.node_id=r.message_node_id),(SELECT count(*) FROM event WHERE id='reply-admission-'||r.message_node_id) FROM receipt r WHERE r.message_node_id=?1",
            [&id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        )?)
    }).await.unwrap();
    assert_eq!(
        first_admitted,
        ("admitted".into(), true, 0),
        "human question admission is not a self-reply model notice"
    );
    let first_reply = store
        .execute(
            Request::new(
                "fixture:reply-after-admission",
                Command::MessageReply(MessageReplyArgs {
                    message_id: first_id.clone(),
                    text: "first synthetic answer".into(),
                    sender_label: None,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(first_reply.result["reply_to"], first_id);
    let first_reply_id = first_reply.result["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    admit_at_real_boundary(&store, vec![first_reply_id.clone()]).await;

    let second = store
        .execute(
            ask("question-second", "second synthetic question"),
            Some(origin("t")),
        )
        .await
        .unwrap();
    let second_id = second.result["message_id"].as_str().unwrap().to_owned();
    let second_reply = store
        .execute(
            Request::new(
                "fixture:reply-before-admission",
                Command::MessageReply(MessageReplyArgs {
                    message_id: second_id.clone(),
                    text: "second synthetic answer".into(),
                    sender_label: None,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(second_reply.result["reply_to"], second_id);
    let second_reply_id = second_reply.result["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let late_cancel = store
        .execute(
            request("cancel-replied-question", &[second_id.as_str()]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(late_cancel.result["items"][0]["outcome"], "unavailable");
    let late_cancel_state: (String, String) = store
        .database
        .call({
            let id = second_id.clone();
            move |db| {
                let admission: String = db.query_row(
                    "SELECT path_admission FROM receipt WHERE message_node_id=?1",
                    [&id],
                    |row| row.get(0),
                )?;
                Ok((
                    admission,
                    crate::control::db::named_reply_state(db, &id).unwrap(),
                ))
            }
        })
        .await
        .unwrap();
    assert_eq!(late_cancel_state, ("unadmitted".into(), "replied".into()));
    let before_attachment: (String,String,String,bool,bool) = store.database.call({
        let id=second_id.clone();
        move |db| {
            let (state,admission,in_path,has_draft):(String,String,bool,bool)=db.query_row(
                "SELECT r.state,r.path_admission,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id='t' AND p.node_id=r.message_node_id),EXISTS(SELECT 1 FROM draft d WHERE json_extract(d.body_json,'$.source_message_id')=r.message_node_id) FROM receipt r WHERE r.message_node_id=?1",
                [&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            )?;
            Ok((state,admission,crate::control::db::named_reply_state(db,&id).unwrap(),in_path,has_draft))
        }
    }).await.unwrap();
    assert_eq!(
        before_attachment,
        (
            "applied".into(),
            "unadmitted".into(),
            "replied".into(),
            false,
            false
        )
    );

    // Accept a real attachment operation after Q and R; driver first-boundary processing
    // must merge Q/R/A before any of them is selected.
    let attachment_path = root.path().join("ordering-a.txt");
    std::fs::write(&attachment_path, b"synthetic attachment A").unwrap();
    let attachments = crate::attachments::AttachmentService::new(
        store.database.clone(),
        &store.config,
        root.path().into(),
        "fixture".into(),
    );
    let plan = attachments
        .plan_add(
            "t",
            vec![crate::attachments::AttachmentInput {
                path: attachment_path.clone(),
                kind: AttachmentKind::File,
            }],
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let attachment_operation = "fixture:attachment-a";
    store
        .execute_with_plan(
            Request::new(
                attachment_operation,
                Command::AttachmentAdd(AttachmentAddArgs {
                    thread_id: Some("t".into()),
                    path: attachment_path.to_string_lossy().into_owned(),
                    kind: AttachmentKind::File,
                }),
            ),
            None,
            plan,
        )
        .await
        .unwrap();
    let attachment_id = store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT node_id FROM attachment_delivery WHERE thread_id='t' AND operation_id=?1",
                [attachment_operation],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    let applied = store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let applied = super::super::attachment_delivery::apply_pending_tx(
                &tx,
                "t",
                &chrono::Utc::now().to_rfc3339(),
            )?;
            tx.commit()?;
            Ok(applied)
        })
        .await
        .unwrap();
    assert_eq!(
        applied, 1,
        "the existing eligible attachment frontier applies one A"
    );
    let selected: Vec<String> = store
        .database
        .call(|db| {
            let mut statement =
                db.prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?;
            statement
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
        .unwrap();
    let positions = [
        first_id.clone(),
        first_reply_id,
        second_id.clone(),
        second_reply_id,
        attachment_id,
    ];
    let indexes: Vec<usize> = positions
        .iter()
        .map(|id| selected.iter().position(|node| node == id).unwrap())
        .collect();
    assert!(
        indexes.windows(2).all(|pair| pair[0] < pair[1]),
        "actual Store Q/R and AttachmentAdd A enter selected path in source order: {selected:?}"
    );
    attachments.wait_idle().await.unwrap();
    let second_notice: i64 = store
        .database
        .call({
            let id = second_id.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM event WHERE id='reply-admission-'||?1",
                    [id],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(
        second_notice, 0,
        "admitting a question never directs the model to reply to itself"
    );

    let cancelled = store
        .execute(
            ask("question-cancel", "cancelled synthetic question"),
            Some(origin("t")),
        )
        .await
        .unwrap();
    let cancelled_id = cancelled.result["message_id"].as_str().unwrap().to_owned();
    let outcome = store
        .execute(request("cancel-question", &[cancelled_id.as_str()]), None)
        .await
        .unwrap();
    assert_eq!(outcome.result["items"][0]["outcome"], "cancelled");
    let audit: (String,String,String,String,String,String) = store.database.call({
        let id=cancelled_id.clone();
        move |db| {
            let (receipt,operation,state,admission,body):(String,String,String,String,String)=db.query_row(
                "SELECT r.id,r.client_operation_id,r.state,r.path_admission,d.body_json FROM receipt r JOIN draft d ON d.state='archived' AND json_extract(d.body_json,'$.source_message_id')=r.message_node_id WHERE r.message_node_id=?1",
                [&id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            )?;
            Ok((receipt,operation,state,admission,body,crate::control::db::named_reply_state(db,&id).unwrap()))
        }
    }).await.unwrap();
    assert_eq!(audit.2, "cancelled");
    assert_eq!(audit.3, "unadmitted");
    assert_eq!(audit.5, "cancelled");
    let draft: serde_json::Value = serde_json::from_str(&audit.4).unwrap();
    assert_eq!(draft["source_message_id"], cancelled_id);
    assert_eq!(draft["source_receipt_id"], audit.0);
    assert_eq!(draft["source_operation_id"], audit.1);
    assert!(!selected.contains(&cancelled_id));
    store.database.shutdown().await.unwrap();
    drop(store);

    // This asserts receipt/draft durability only, not the full Control shutdown split.
    let reopened = DbWorker::open(root.path().join("pending.db"))
        .await
        .unwrap();
    let persisted: (String,String,String,String) = reopened.call({
        let id=cancelled_id.clone();
        move |db| db.query_row(
            "SELECT r.id,r.client_operation_id,r.state,r.path_admission FROM receipt r WHERE r.message_node_id=?1 AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id)",
            [&id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).map_err(Into::into)
    }).await.unwrap();
    assert_eq!(persisted.0, audit.0);
    assert_eq!(persisted.1, audit.1);
    assert_eq!(persisted.2, "cancelled");
    assert_eq!(persisted.3, "unadmitted");
    reopened.shutdown().await.unwrap();
}
#[tokio::test]
async fn held_frontier_defers_the_replied_question_and_later_attachment() {
    let (root, store) = store().await;
    // An unassigned ordinary message retained across a restart, not a Revisit.
    let held_id = "interrupted-input".to_owned();
    store.database.call(|db| {
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json) VALUES('interrupted-input','user','0','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,hold_reason) VALUES('interrupted-input','interrupted-input','null','t','interrupted-input','held','0','previous_instance_interrupted');")?;
        Ok(())
    }).await.unwrap();

    let question = store
        .execute(
            Request::new(
                "fixture:held-frontier-question",
                Command::MessageAsk(MessageAskArgs {
                    human: false,
                    sender_label: None,
                    thread_id: None,
                    to: None,
                    deliver: Deliver::Next,
                    text: "Q behind H".into(),
                }),
            ),
            Some(origin("t")),
        )
        .await
        .unwrap();
    let question_id = question.result["message_id"].as_str().unwrap().to_owned();
    let reply = store
        .execute(
            Request::new(
                "fixture:held-frontier-reply",
                Command::MessageReply(MessageReplyArgs {
                    message_id: question_id.clone(),
                    text: "R depends on Q".into(),
                    sender_label: None,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    let reply_id = reply.result["message_id"].as_str().unwrap().to_owned();

    let path = root.path().join("held-frontier-a.txt");
    std::fs::write(&path, b"synthetic attachment behind R").unwrap();
    let attachments = crate::attachments::AttachmentService::new(
        store.database.clone(),
        &store.config,
        root.path().into(),
        "fixture".into(),
    );
    let plan = attachments
        .plan_add(
            "t",
            vec![crate::attachments::AttachmentInput {
                path: path.clone(),
                kind: AttachmentKind::File,
            }],
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    store
        .execute_with_plan(
            Request::new(
                "fixture:held-frontier-attachment",
                Command::AttachmentAdd(AttachmentAddArgs {
                    thread_id: Some("t".into()),
                    path: path.to_string_lossy().into_owned(),
                    kind: AttachmentKind::File,
                }),
            ),
            None,
            plan,
        )
        .await
        .unwrap();
    let attachment_id = store.database.call(|db| Ok(db.query_row(
        "SELECT node_id FROM attachment_delivery WHERE thread_id='t' AND operation_id='fixture:held-frontier-attachment'",
        [], |r| r.get::<_,String>(0),
    )?)).await.unwrap();

    let (admitted, deferred, deferred_replies, applied, states, selected) = store.database.call({
        let held_id = held_id.clone();
        let reply_id = reply_id.clone();
        let question_id = question_id.clone();
        let attachment_id = attachment_id.clone();
        move |db| {
            let tx = db.transaction()?;
            let revision = crate::history::selection_revision(&tx,"t",crate::history::ThreadScope::Live)?;
            let admission = crate::control::message_delivery::plan_admission_tx(
                &tx,"t",std::slice::from_ref(&reply_id),"bounded",
            )?;
            assert!(admission.admitted_nodes.is_empty());
            assert!(admission.deferred_reply_nodes.contains(&reply_id));
            crate::control::message_delivery::append_admission_plan_tx(
                &tx,"t",revision as u64,&admission,"bounded",
            )?;
            let applied = super::super::attachment_delivery::apply_pending_tx(&tx,"t","bounded")?;
            let states: (String,String,String) = tx.query_row(
                "SELECT (SELECT state FROM receipt WHERE message_node_id=?1),(SELECT path_admission FROM receipt WHERE message_node_id=?2),(SELECT status FROM attachment_delivery WHERE node_id=?3)",
                params![held_id,question_id,attachment_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            )?;
            let selected: Vec<String> = tx.prepare("SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal")?
                .query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            tx.commit()?;
            Ok((admission.admitted_callers,admission.deferred_callers,admission.deferred_reply_nodes,applied,states,selected))
        }
    }).await.unwrap();
    assert!(admitted.is_empty());
    assert_eq!(deferred.as_slice(), std::slice::from_ref(&reply_id));
    assert_eq!(deferred_replies.as_slice(), std::slice::from_ref(&reply_id));
    assert_eq!(applied, 0);
    assert_eq!(states.0, "held");
    assert_eq!(states.1, "unadmitted");
    assert_eq!(states.2, "accepted");
    assert!(!selected.contains(&question_id));
    assert!(!selected.contains(&reply_id));
    assert!(!selected.contains(&attachment_id));
    attachments.wait_idle().await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn bounded_admission_prefix_keeps_the_129th_question_before_its_reply() {
    const DUTIES: usize = 129;
    let (_root, store) = store().await;
    let mut questions = Vec::with_capacity(DUTIES);
    let mut replies = Vec::with_capacity(DUTIES);
    for index in 0..DUTIES {
        let ask = store
            .execute(
                Request::new(
                    format!("fixture:question-{index}"),
                    Command::MessageAsk(MessageAskArgs {
                        human: false,
                        sender_label: None,
                        thread_id: None,
                        to: None,
                        deliver: Deliver::Next,
                        text: format!("Q{index}"),
                    }),
                ),
                Some(origin("t")),
            )
            .await
            .unwrap();
        let accepted: MessageAccepted = serde_json::from_value(ask.result).unwrap();
        assert_eq!(accepted.delivery_status, DeliveryStatus::Accepted);
        assert_eq!(accepted.assignment, Assignment::Pending);
        let question = accepted.message_id;
        let answer = store
            .execute(
                Request::new(
                    format!("fixture:reply-{index}"),
                    Command::MessageReply(MessageReplyArgs {
                        message_id: question.clone(),
                        text: format!("R{index}"),
                        sender_label: None,
                    }),
                ),
                None,
            )
            .await
            .unwrap();
        questions.push(question);
        replies.push(answer.result["message_id"].as_str().unwrap().to_owned());
    }
    let before_plan: (i64, i64) = store.database.call(|db| {
        let owed = |extra: &str| {
            let counts = crate::control::db::named_reply_counts(db, &format!("target_thread_id='t' AND state='applied' AND path_admission='unadmitted'{extra} AND json_extract(result_json,'$.to_human')=1"));
            counts.get("pending").copied().unwrap_or(0) + counts.get("replied").copied().unwrap_or(0)
        };
        Ok((owed(""), owed(" AND run_id IS NULL AND turn_id IS NULL")))
    }).await.unwrap();
    assert_eq!(before_plan, (DUTIES as i64, DUTIES as i64));
    let (plan, selected) = store
        .database
        .call({
            let replies = replies.clone();
            let last_question = questions[DUTIES - 1].clone();
            move |db| {
                let tx = db.transaction()?;
                let revision = crate::history::selection_revision(
                    &tx,
                    "t",
                    crate::history::ThreadScope::Live,
                )?;
                let plan = crate::control::message_delivery::plan_admission_tx(
                    &tx, "t", &replies, "bounded",
                )?;
                assert_eq!(plan.admitted_callers.len(), DUTIES);
                assert!(plan.deferred_callers.is_empty());
                assert!(plan.admitted_nodes.contains(&last_question));
                crate::control::message_delivery::append_admission_plan_tx(
                    &tx,
                    "t",
                    revision as u64,
                    &plan,
                    "bounded",
                )?;
                let selected: Vec<String> = tx
                    .prepare(
                        "SELECT node_id FROM thread_path WHERE thread_id='t' ORDER BY ordinal",
                    )?
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                tx.commit()?;
                Ok((plan, selected))
            }
        })
        .await
        .unwrap();
    assert_eq!(selected.len(), DUTIES * 2);
    assert!(
        selected
            .iter()
            .all(|node| !node.starts_with("event-reply-admission-"))
    );
    for (question, reply) in questions.iter().zip(&replies) {
        let q = selected
            .iter()
            .position(|node| node == question)
            .expect("question admitted");
        let r = selected
            .iter()
            .position(|node| node == reply)
            .expect("reply admitted");
        assert!(q < r, "each answer follows its own question");
    }
    assert!(plan.admitted_nodes.contains(&questions[DUTIES - 1]));
    let after_plan = store.database.call({
        let questions = questions.clone();
        move |db| {
            questions.into_iter().map(|question| db.query_row(
                "SELECT r.state,r.path_admission,EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=r.target_thread_id AND p.node_id=r.message_node_id),json_extract(r.result_json,'$.reply.state') FROM receipt r WHERE r.message_node_id=?1",
                [question],
                |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,bool>(2)?, row.get::<_,String>(3)?)),
            )).collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }
    }).await.unwrap();
    assert_eq!(after_plan.len(), DUTIES);
    assert!(
        after_plan
            .iter()
            .all(|state| state == &("applied".into(), "admitted".into(), true, "replied".into()))
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_is_partial_unrestricted_archived_idempotent_and_never_wakes_models() {
    let (_root, store) = store().await;
    store.database.call(|db| {
        input(db,"own",Some(&origin("different-sender")))?; input(db,"human",None)?; input(db,"human-late",None)?;
        input(db,"rewound",Some(&origin("sender")))?; input(db,"selected",Some(&origin("sender")))?;
        input(db,"missing",Some(&origin("sender")))?; input(db,"uncertain",Some(&origin("sender")))?;
        db.execute("UPDATE receipt SET state='held',path_admission='unknown',hold_reason='uncertain_path_admission' WHERE id='uncertain'",[])?;
        db.execute_batch("UPDATE receipt SET path_admission='admitted' WHERE id='rewound'; INSERT INTO thread_path VALUES('t',0,'selected'); UPDATE thread SET cursor_node_id='selected' WHERE id='t'; UPDATE node SET payload_json=NULL,deleted_reason='collected' WHERE id='missing';")?; Ok(())
    }).await.unwrap();
    let req = request(
        "cancel-partial",
        &[
            "own",
            "human",
            "rewound",
            "selected",
            "missing",
            "absent",
            "uncertain",
        ],
    );
    let outcome = store
        .execute(req.clone(), Some(origin("sender")))
        .await
        .unwrap();
    assert!(outcome.quiet_wake_threads.is_empty());
    assert_eq!(outcome.changed_threads, ["t"]);
    let cancelled: MessageCancelResult = serde_json::from_value(outcome.result.clone()).unwrap();
    assert_eq!(
        cancelled
            .items
            .iter()
            .map(|i| i.outcome)
            .collect::<Vec<_>>(),
        [
            MessageCancelOutcome::Cancelled,
            MessageCancelOutcome::Cancelled,
            MessageCancelOutcome::AlreadyAdmitted,
            MessageCancelOutcome::AlreadyAdmitted,
            MessageCancelOutcome::Unavailable,
            MessageCancelOutcome::NotFound,
            MessageCancelOutcome::Cancelled
        ]
    );
    assert_eq!(cancelled.items[1].thread_id.as_deref(), Some("t"));
    let replay = store.execute(req, Some(origin("sender"))).await.unwrap();
    assert_eq!(replay.result, outcome.result);
    assert!(replay.changed_threads.is_empty());
    let already = store
        .execute(
            request("cancel-again", &["own"]),
            Some(origin("different-sender")),
        )
        .await
        .unwrap();
    assert_eq!(already.result["items"][0]["outcome"], "forbidden");
    let repeat = store
        .execute(request("cancel-repeat", &["own"]), Some(origin("sender")))
        .await
        .unwrap();
    assert_eq!(repeat.result["items"][0]["outcome"], "forbidden");
    let human = store
        .execute(request("human-cancel", &["human-late"]), None)
        .await
        .unwrap();
    assert_eq!(human.result["items"][0]["outcome"], "cancelled");
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='archived'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                4
            );
            assert_eq!(
                db.query_row(
                    "SELECT run_id,turn_id,path_admission FROM receipt WHERE id='own'",
                    [],
                    |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?
                    ))
                )?,
                ("r".into(), "u".into(), "unadmitted".into())
            );
            let body: String = db.query_row(
                "SELECT body_json FROM draft WHERE id='cancelled-message:own'",
                [],
                |r| r.get(0),
            )?;
            assert!(!body.contains("private-origin-token"));
            assert!(!body.contains("payload_json"));
            assert_eq!(
                db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))?,
                0,
                "cancellation has no model notification"
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancellation_and_admission_commit_boundary_prevents_retraction_or_partial_storage_success()
{
    let (_root, store) = store().await;
    store
        .database
        .call(|db| {
            input(db, "first", None)?;
            input(db, "second", None)?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .execute(request("cancel-first", &["first"]), None)
        .await
        .unwrap();
    store
        .database
        .call(|db| {
            let tx = db.transaction()?;
            let pending: Vec<String> = tx
                .prepare(
                    "SELECT message_node_id FROM receipt WHERE state='accepted' ORDER BY rowid",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(pending, ["second"]);
            crate::history::append_existing_nodes(&tx, "t", 0, &pending, "admit")?;
            crate::control::message_delivery::mark_admitted_tx(&tx, "t", &pending, "admit")?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .execute(request("too-late", &["second"]), None)
            .await
            .unwrap()
            .result["items"][0]["outcome"],
        "already_admitted"
    );
    store.database.call(|db| {
        input(db,"third",None)?; input(db,"fourth",None)?;
        db.execute_batch("CREATE TRIGGER fail_cancel_archive BEFORE INSERT ON draft WHEN new.id='cancelled-message:fourth' BEGIN SELECT RAISE(ABORT,'synthetic archive failure'); END;")?; Ok(())
    }).await.unwrap();
    assert!(
        store
            .execute(request("storage-fail", &["third", "fourth"]), None)
            .await
            .is_err()
    );
    store.database.call(|db| {
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE id IN ('third','fourth') AND state='accepted'",[],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(db.query_row("SELECT count(*) FROM draft WHERE id='cancelled-message:third'",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}
fn submission(op: &str, source: &str, revision: u64, text: &str) -> DraftSubmission {
    DraftSubmission {
        draft_id: DraftSubmission::id_for_operation(op),
        source_draft_id: source.into(),
        revision,
        snapshot: json!({"draft_id":source,"revision":revision,"text":text,"cursor":text.len(),"attachments":[],"folds":[],"literal_paste":false,"edit":null}),
    }
}
#[tokio::test]
async fn frozen_submission_survives_precommit_failure_and_accepts_atomically_without_touching_newer_editable_draft()
 {
    let (root, store) = store().await;
    let frozen = submission("fixture:frozen", "editable", 7, "frozen send");
    store
        .prepare_draft_submission("fixture:frozen", Some("t"), &frozen)
        .await
        .unwrap();
    store
        .prepare_draft_submission("fixture:frozen", Some("t"), &frozen)
        .await
        .unwrap();
    let conflicting = submission("fixture:frozen", "editable", 7, "changed content");
    assert_eq!(
        store
            .prepare_draft_submission("fixture:frozen", Some("t"), &conflicting)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    store.database.call(|db| {
        db.execute("INSERT INTO draft VALUES('editable','t',8,?1,'editing','later')",[json!({"draft_id":"editable","revision":8,"text":"newer unsent edit","attachments":[]}).to_string()])?;
        db.execute_batch("CREATE TRIGGER fail_frozen_send BEFORE INSERT ON receipt WHEN new.client_operation_id='fixture:frozen' BEGIN SELECT RAISE(ABORT,'synthetic precommit failure'); END;")?; Ok(())
    }).await.unwrap();
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_thread_id: None,
    };
    let options = DraftSendOptions {
        deliver: Deliver::Next,
        submission: Some(frozen.clone()),
        expected_selection_revision: Some(0),
        ..Default::default()
    };
    assert!(
        store
            .send_draft_content(
                "fixture:frozen".into(),
                args.clone(),
                Some("t".into()),
                crate::attachments::DraftContent::appended("frozen send".into(), 0),
                vec![],
                options.clone()
            )
            .await
            .is_err()
    );
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='submitting'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                0
            );
            db.execute_batch("DROP TRIGGER fail_frozen_send;")?;
            Ok(())
        })
        .await
        .unwrap();
    // A different operation has a different frozen identity; the failed operation remains immutable.
    let frozen = submission("fixture:accepted", "editable", 7, "frozen send");
    store
        .prepare_draft_submission("fixture:accepted", Some("t"), &frozen)
        .await
        .unwrap();
    let options = DraftSendOptions {
        deliver: Deliver::Next,
        submission: Some(frozen.clone()),
        expected_selection_revision: Some(0),
        ..Default::default()
    };
    let accepted = store
        .send_draft_content(
            "fixture:accepted".into(),
            args.clone(),
            Some("t".into()),
            crate::attachments::DraftContent::appended("frozen send".into(), 0),
            vec![],
            options.clone(),
        )
        .await
        .unwrap();
    let replay = store
        .send_draft_content(
            "fixture:accepted".into(),
            args,
            Some("t".into()),
            crate::attachments::DraftContent::appended("frozen send".into(), 0),
            vec![],
            options,
        )
        .await
        .unwrap();
    assert_eq!(replay.result, accepted.result);
    let frozen_id = frozen.draft_id.clone();
    let expected = accepted.result["message_id"].clone();
    store
        .database
        .call(move |db| {
            let (body, state): (String, String) = db.query_row(
                "SELECT body_json,state FROM draft WHERE id=?1",
                [frozen_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(state, "submitted");
            assert_eq!(body["_submission"]["message_id"], expected);
            assert_eq!(body["text"], "frozen send");
            assert_eq!(
                db.query_row(
                    "SELECT state,revision FROM draft WHERE id='editable'",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                )?,
                ("editing".into(), 8)
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
    let reopened = DbWorker::open(root.path().join("pending.db"))
        .await
        .unwrap();
    reopened
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='submitted'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn frozen_first_send_survives_without_thread_and_binds_only_at_acceptance() {
    let (_root, store) = store().await;
    let frozen = submission("fixture:first-thread", "new-composer", 3, "first request");
    store
        .prepare_draft_submission("fixture:first-thread", None, &frozen)
        .await
        .unwrap();
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state='submitting' AND thread_id IS NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    let accepted = store
        .send_draft_content(
            "fixture:first-thread".into(),
            ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_thread_id: None,
            },
            None,
            crate::attachments::DraftContent::appended("first request".into(), 0),
            vec![],
            DraftSendOptions {
                submission: Some(frozen.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let thread = accepted.result["thread_id"].as_str().unwrap().to_owned();
    store
        .database
        .call(move |db| {
            let (saved, state): (String, String) = db.query_row(
                "SELECT thread_id,state FROM draft WHERE id=?1",
                [frozen.draft_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(saved, thread);
            assert_eq!(state, "submitted");
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_gc_protects_frozen_and_archived_sources_until_their_thread_is_deleted() {
    let (root, store) = store().await;
    let directory = root.path().join("managed/clipboard");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("managed.png");
    std::fs::write(&file, b"fixture").unwrap();
    let file_text = file.to_string_lossy().into_owned();
    store.database.call(move|db| {
        db.execute("INSERT INTO managed_file(id,absolute_path,state,raw_size,created_at)VALUES('managed',?1,'ready',7,'2000-01-01')",[&file_text])?;
        db.execute("INSERT INTO draft VALUES('frozen','t',1,?1,'submitting','0')",[json!({"attachments":[{"id":"managed","path":file_text}]}).to_string()])?;Ok(())
    }).await.unwrap();
    let attachments = crate::attachments::AttachmentService::new(
        store.database.clone(),
        &store.config,
        root.path().into(),
        "fixture".into(),
    );
    for state in ["submitting", "submitted"] {
        store
            .database
            .call(move |db| {
                db.execute("UPDATE draft SET state=?1 WHERE id='frozen'", [state])?;
                Ok(())
            })
            .await
            .unwrap();
        let report = attachments
            .collect_managed(chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(report.protected_bytes, 7);
        assert!(report.deleted.is_empty());
        assert!(file.exists());
    }
    let file_text = file.to_string_lossy().into_owned();
    store.database.call(move|db| {
        db.execute("DELETE FROM draft WHERE id='frozen'",[])?; input(db,"archive-source",None)?;
        db.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('reference','archive-source',?1,'image','managed_clipboard','0')",[file_text])?;Ok(())
    }).await.unwrap();
    store
        .execute(request("archive-source", &["archive-source"]), None)
        .await
        .unwrap();
    let report = attachments
        .collect_managed(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report.protected_bytes, 7);
    assert!(report.deleted.is_empty());
    assert!(file.exists());
    store
        .database
        .call(|db| {
            db.execute("UPDATE thread SET deleted_at='deleted' WHERE id='t'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let report = attachments
        .collect_managed(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report.protected_bytes, 0);
    assert_eq!(report.deleted, ["managed"]);
    assert!(!file.exists());
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn actual_control_draft_replay_precedes_missing_files_and_old_owner_checks() {
    use crate::{app::DataOwner, control::Control};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\nresponse_max_retries=0\n[limits]\nattachment_file_bytes=1024\nattachment_request_bytes=1024\n").unwrap();
    let data = root.path().join("data");
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let operation = format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4());
    let old_instance = control.store.instance_id.clone();
    let file = root.path().join("original.txt");
    std::fs::write(&file, b"fixture").unwrap();
    let files = vec![crate::attachments::AttachmentInput {
        path: file.clone(),
        kind: AttachmentKind::File,
    }];
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_thread_id: None,
    };
    let mut frozen = submission(&operation, "composer", 3, "saved request\u{fffc}");
    frozen.snapshot["attachments"] = json!([{"id":"original-chip","path":file,"kind":"file","state":"active","position":"saved request".len()}]);
    let options = DraftSendOptions {
        submission: Some(frozen.clone()),
        ..Default::default()
    };
    let accepted = control
        .send_ordered_draft(
            operation.clone(),
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options.clone(),
        )
        .await
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    let replay = control
        .send_ordered_draft(
            operation.clone(),
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay, accepted);
    // Changed file bytes do not change the identity of the original request.
    std::fs::write(&file, vec![0; 2048]).unwrap();
    let replay = control
        .send_ordered_draft(
            operation.clone(),
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options.clone(),
        )
        .await
        .unwrap();
    assert_eq!(replay, accepted);
    for field in ["now", "text", "snapshot", "files", "args", "thread"] {
        let mut changed = options.clone();
        let mut changed_args = args.clone();
        let mut changed_files = files.clone();
        let mut changed_thread = None;
        let text = if field == "text" {
            "different request"
        } else {
            "saved request"
        };
        match field {
            "now" => changed.deliver = crate::protocol::Deliver::Now,
            "snapshot" => {
                changed.submission.as_mut().unwrap().snapshot["text"] =
                    json!("different frozen source")
            }
            "files" => changed_files[0].path = root.path().join("different.txt"),
            "args" => changed_args.model = Some("different-model".into()),
            "thread" => changed_thread = Some("different-thread".into()),
            _ => {}
        }
        let error = control
            .send_ordered_draft(
                operation.clone(),
                changed_args,
                changed_thread,
                crate::attachments::DraftContent::appended(text.into(), 1),
                changed_files,
                changed,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::OperationConflict,
            "changed {field} cannot return a saved different request"
        );
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    assert_ne!(control.store.instance_id, old_instance);
    let before = owner
        .db
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM run", [], |r| r.get::<_, i64>(0))?,
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    let replay = control
        .send_ordered_draft(
            operation,
            args.clone(),
            None,
            crate::attachments::DraftContent::appended("saved request".into(), 1),
            files.clone(),
            options,
        )
        .await
        .unwrap();
    assert_eq!(
        replay, accepted,
        "known old-owner operation replays before frozen owner validation"
    );
    assert!(
        control.index_workers.get().is_none(),
        "saved replay does not start background workers"
    );
    let after = owner
        .db
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM run", [], |r| r.get::<_, i64>(0))?,
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(after, before, "saved replay has no mutation/driver effects");
    let expired = control
        .send_ordered_draft(
            format!("{old_instance}:unknown"),
            args,
            None,
            crate::attachments::DraftContent::appended("unknown".into(), 1),
            files,
            DraftSendOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(expired.code, ErrorCode::OperationExpired);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn frozen_budget_preserves_tiny_raw_text_limits_and_json_escaping_with_separate_metadata_bound()
 {
    let (root, base) = store().await;
    let mut config = (*base.config).clone();
    config.limits.draft_bytes = 128;
    let store = Store::new(
        base.database.clone(),
        Arc::new(config),
        root.path().into(),
        "fixture".into(),
    );
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_thread_id: None,
    };
    for (n, text) in [
        "hi".into(),
        "a".repeat(128),
        "\u{0001}".repeat(128),
        "\"".repeat(128),
    ]
    .into_iter()
    .enumerate()
    {
        let operation = format!("fixture:raw-budget-{n}");
        let frozen = submission(&operation, &format!("source-{n}"), 1, &text);
        store
            .prepare_draft_submission(&operation, Some("t"), &frozen)
            .await
            .unwrap();
        let accepted = store
            .send_draft_content(
                operation,
                args.clone(),
                Some("t".into()),
                crate::attachments::DraftContent::appended(text, 0),
                vec![],
                DraftSendOptions {
                    submission: Some(frozen.clone()),
                    expected_selection_revision: Some(0),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(accepted.result["message_id"].is_string());
        store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row(
                        "SELECT state FROM draft WHERE id=?1",
                        [&frozen.draft_id],
                        |r| r.get::<_, String>(0)
                    )?,
                    "submitted"
                );
                let serialized: String = db.query_row(
                    "SELECT body_json FROM draft WHERE id=?1",
                    [&frozen.draft_id],
                    |row| row.get(0),
                )?;
                assert!(
                    serialized.len() > 128,
                    "saved metadata and escaping use a separate budget"
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    let too_much_text = submission("fixture:raw-too-long", "source-long", 1, &"a".repeat(129));
    let error = store
        .prepare_draft_submission("fixture:raw-too-long", Some("t"), &too_much_text)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert!(error.message.contains("text"));
    let mut too_much_metadata = submission("fixture:metadata-too-long", "source-metadata", 1, "a");
    too_much_metadata.snapshot["extra_metadata"] = json!("z".repeat(MAX_FRAME_BYTES + 4096));
    let error = store
        .prepare_draft_submission("fixture:metadata-too-long", Some("t"), &too_much_metadata)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert!(error.message.contains("metadata"));
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelling_source_reference_is_independent_of_the_current_raw_draft_text_limit() {
    let (root, base) = store().await;
    base.database
        .call(|db| {
            input(db, "cancel-with-small-budget", None)?;
            Ok(())
        })
        .await
        .unwrap();
    let mut config = (*base.config).clone();
    config.limits.draft_bytes = 1;
    let store = Store::new(
        base.database.clone(),
        Arc::new(config),
        root.path().into(),
        "fixture".into(),
    );
    let result = store
        .execute(request("tiny-cancel", &["cancel-with-small-budget"]), None)
        .await
        .unwrap();
    assert_eq!(result.result["items"][0]["outcome"], "cancelled");
    store.database.call(|db| {
        let bytes:i64=db.query_row("SELECT octet_length(body_json) FROM draft WHERE id='cancelled-message:cancel-with-small-budget'",[],|r|r.get(0))?;
        assert!(bytes>1 && bytes<=MAX_FRAME_BYTES as i64);Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn admitted_copy_retires_atomically_replay_stays_exact_and_storage_does_not_keep_two_bodies()
{
    let (root, store) = store().await;
    let attachments = crate::attachments::AttachmentService::new(
        store.database.clone(),
        &store.config,
        root.path().into(),
        "fixture".into(),
    );
    let gc = crate::gc::GcService::new(
        store.database.clone(),
        store.config.clone(),
        root.path().into(),
        Arc::new(attachments),
    );
    for n in 0..16 {
        let operation = format!("fixture:retention-{n}");
        let text = format!("submitted text {n} {}", "x".repeat(8192));
        let frozen = submission(&operation, "composer", n, &text);
        store
            .prepare_draft_submission(&operation, Some("t"), &frozen)
            .await
            .unwrap();
        let args = ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_thread_id: None,
        };
        let options = DraftSendOptions {
            submission: Some(frozen.clone()),
            ..Default::default()
        };
        let content = crate::attachments::DraftContent::appended(text.clone(), 0);
        let accepted = store
            .send_draft_content(
                operation.clone(),
                args.clone(),
                Some("t".into()),
                content.clone(),
                vec![],
                options.clone(),
            )
            .await
            .unwrap();
        let before = gc.status().await.unwrap().logical_live_bytes;
        let message = accepted.result["message_id"].as_str().unwrap().to_owned();
        let selected = message.clone();
        store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let ordinal: i64 = tx.query_row(
                    "SELECT COALESCE(max(ordinal),-1)+1 FROM thread_path WHERE thread_id='t'",
                    [],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('t',?1,?2)",
                    params![ordinal, selected],
                )?;
                crate::control::message_delivery::mark_admitted_tx(&tx, "t", &[selected], "admit")?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let after = gc.status().await.unwrap().logical_live_bytes;
        assert!(
            before >= after + 8192,
            "the whole submitted payload is reclaimed, not just hidden from UI"
        );
        let replay = store
            .send_draft_content(
                operation.clone(),
                args,
                Some("t".into()),
                content,
                vec![],
                options,
            )
            .await
            .unwrap();
        assert_eq!(replay.result, accepted.result);
        // Preparation replay itself must not recreate a retired copy.
        store
            .prepare_draft_submission(&operation, Some("t"), &frozen)
            .await
            .unwrap();
        store
            .database
            .call(move |db| {
                assert!(!db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft WHERE state='submitted')",
                    [],
                    |r| r.get::<_, bool>(0)
                )?);
                let saved: String = db.query_row(
                    "SELECT result_json FROM receipt WHERE client_operation_id=?1",
                    [operation],
                    |r| r.get(0),
                )?;
                let value: Value = serde_json::from_str(&saved).unwrap();
                assert_eq!(value["draft_submission"]["message_id"], message);
                assert!(!saved.contains("submitted text"));
                assert_eq!(
                    db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                    n as i64 + 1
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    store.database.shutdown().await.unwrap();
    let reopened = DbWorker::open(root.path().join("pending.db"))
        .await
        .unwrap();
    reopened.call(|db| {
        assert!(!db.query_row("SELECT EXISTS(SELECT 1 FROM draft WHERE state='submitted')",[],|r|r.get::<_,bool>(0))?);
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE json_type(result_json,'$.draft_submission')='object'",[],|r|r.get::<_,i64>(0))?,16);Ok(())
    }).await.unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn submitting_and_unknown_copies_survive_collection_and_database_reopen() {
    let (root, store) = store().await;
    let frozen = submission(
        "fixture:uncommitted-copy",
        "composer",
        4,
        "recover this unsent text",
    );
    store
        .prepare_draft_submission("fixture:uncommitted-copy", Some("t"), &frozen)
        .await
        .unwrap();
    store.database.call(|db| {
        input(db,"uncertain-copy",None)?;
        db.execute("UPDATE receipt SET path_admission='unknown',state='cancelled' WHERE id='uncertain-copy'",[])?;
        let metadata=json!({"text":"unknown acceptance copy","_submission":{"operation_id":"uncertain-copy","source_draft_id":"another","source_revision":3,"message_id":"uncertain-copy","accepted_thread_id":"t"}});
        db.execute("INSERT INTO draft VALUES(?1,'t',3,?2,'submitted','0')",params![DraftSubmission::id_for_operation("uncertain-copy"),metadata.to_string()])?;
        let tx=db.transaction()?;assert_eq!(crate::control::submission_retention::collect_batch(&tx)?,0);tx.commit()?;Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
    let reopened = DbWorker::open(root.path().join("pending.db"))
        .await
        .unwrap();
    reopened
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM draft WHERE state IN ('submitting','submitted')",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                2
            );
            let body: String = db.query_row(
                "SELECT body_json FROM draft WHERE state='submitting'",
                [],
                |r| r.get(0),
            )?;
            assert!(body.contains("recover this unsent text"));
            Ok(())
        })
        .await
        .unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn ordinary_history_gc_backfills_markers_and_collects_old_copies_in_batches() {
    let (root, store) = store().await;
    store.database.call(|db| {
        let tx=db.transaction()?;
        for n in 0..130 {
            let op=format!("old-op-{n:03}");let source=format!("old-source-{n:03}");
            input(&tx,&op,None)?;
            tx.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE id=?1",[&op])?;
            let body=json!({"draft_id":source,"revision":7,"text":"old redundant text","attachments":[],"_submission":{"operation_id":op,"source_draft_id":source,"source_revision":7,"message_id":op,"accepted_thread_id":"t"}}).to_string();
            tx.execute("INSERT INTO draft VALUES(?1,'t',7,?2,'submitted','0')",params![DraftSubmission::id_for_operation(&op),body])?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    let attachments = Arc::new(crate::attachments::AttachmentService::new(
        store.database.clone(),
        &store.config,
        root.path().into(),
        "fixture".into(),
    ));
    let gc = crate::gc::GcService::new(
        store.database.clone(),
        store.config.clone(),
        root.path().into(),
        attachments,
    );
    // One pass runs bounded 128-row units back to back until a short unit
    // (#171), so the second pass finds nothing.
    for expected in [130, 0] {
        let report = gc
            .collect(StorageCategory::History, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(report.submitted_drafts_collected, expected);
    }
    store.database.call(|db| {
        assert_eq!(db.query_row("SELECT count(*) FROM draft WHERE state='submitted'",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE json_type(result_json,'$.draft_submission')='object'",[],|r|r.get::<_,i64>(0))?,130);Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}
