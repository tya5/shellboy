use super::*;
use crate::attachments::PlannedAttachment;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub(in crate::control) async fn fixture(
    url: &str,
) -> (tempfile::TempDir, DataOwner, Arc<Control>, MessageAccepted) {
    fixture_with_policy(url).await
}

async fn fixture_with_policy(
    url: &str,
) -> (tempfile::TempDir, DataOwner, Arc<Control>, MessageAccepted) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url={url:?}\n")).unwrap();
    let owner = DataOwner::start(
        Some(path),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let thread = control
        .store
        .execute(
            Request::new(
                format!("{}:create", owner.instance_id),
                Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    cwd: None,
                    model: None,
                    effort: None,
                    parent_thread_id: None,
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let accepted = control
        .store
        .execute(
            Request::new(
                format!("{}:send", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(thread),
                    text: "original instruction".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    (
        root,
        owner,
        control,
        serde_json::from_value(accepted).unwrap(),
    )
}
pub(super) async fn ready(control: &Arc<Control>, accepted: &MessageAccepted) -> Box<Snapshot> {
    match next(
        control,
        &accepted.thread_id,
        accepted.run_id.as_deref().unwrap(),
    )
    .await
    .unwrap()
    {
        Next::Ready(snapshot) => snapshot,
        _ => panic!("fixture expected runnable snapshot"),
    }
}
async fn add_human_attachment(
    control: &Arc<Control>,
    accepted: &MessageAccepted,
    path: PathBuf,
) -> Value {
    control
        .store
        .send_draft(
            format!("{}:{}", control.store.instance_id, id()),
            ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_thread_id: None,
            },
            Some(accepted.thread_id.clone()),
            "late instruction with attachment".into(),
            vec![PlannedAttachment {
                path,
                kind: AttachmentKind::File,
                observed_bytes: Some(12),
                managed_id: None,
            }],
            None,
        )
        .await
        .unwrap()
        .result
}
pub(super) async fn close(control: Arc<Control>, owner: DataOwner) {
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn outstanding_now_receipt_applies_at_the_normal_driver_boundary() {
    use crate::protocol::{Deliver, WaitArgs, WaitCondition};
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let request = Request::new(
        format!("{}:legacy-now", owner.instance_id),
        Command::MessageSend(MessageSendArgs {
            max_model_requests: None,
            resume_recovery: false,
            human: false,
            sender_label: None,
            deliver: Deliver::Now,
            thread_id: Some(accepted.thread_id.clone()),
            text: "outstanding legacy input".into(),
        }),
    );
    let original = crate::control::db::legacy_now_fixture(&control.store, &request).await;
    let message = original["message_id"].as_str().unwrap();
    assert_eq!(original["deliver"], "now");
    let condition = WaitCondition::MessageDelivered {
        message_id: message.into(),
    };
    let inspect = || {
        super::super::wait::observe(
            control.clone(),
            "legacy-now-observation".into(),
            WaitArgs {
                conditions: vec![condition.clone()],
                timeout_ms: 0,
                after: None,
            },
            None,
            CancellationToken::new(),
        )
    };
    assert_eq!(inspect().await.unwrap()["status"], "timeout");
    let snapshot = ready(&control, &accepted).await;
    let prepared = Arc::new(
        control
            .attachments
            .prepare_at_revision(
                &accepted.thread_id,
                CancellationToken::new(),
                false,
                snapshot.selection,
            )
            .await
            .unwrap(),
    );
    assert!(
        admit_attempt(
            &control,
            &snapshot,
            "legacy-boundary",
            prepared.clone(),
            0,
            0
        )
        .await
        .unwrap()
    );
    assert_eq!(inspect().await.unwrap()["status"], "timeout");
    let response = model::ModelResponse::completed(json!({
        "id":"legacy-response", "status":"completed", "output":[
            {"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"applied"}]}
        ]
    }), true).unwrap();
    model::request_path::persist_received_attempt(
        &control.store.database,
        "legacy-boundary",
        model::request_path::AttemptKind::Conversation,
        false,
        &Ok(response.clone()),
    )
    .await
    .unwrap();
    assert!(
        adopt_response(
            &control,
            &snapshot,
            "legacy-boundary",
            &response,
            prepared,
            None,
            Vec::new()
        )
        .await
        .unwrap()
    );
    assert_eq!(inspect().await.unwrap()["matched"], json!([condition]));
    let replay = control.store.execute(request.clone(), None).await.unwrap();
    assert_eq!(replay.result, original);
    assert!(replay.quiet_wake_threads.is_empty());
    let mut changed = request;
    let Command::MessageSend(args) = &mut changed.command else {
        unreachable!()
    };
    args.deliver = Deliver::Next;
    assert_eq!(
        control.store.execute(changed, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    close(control, owner).await;
}

#[tokio::test]
async fn human_acceptance_between_next_and_prepare_refreshes_source_before_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (root, owner, control, accepted) =
        fixture(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let snapshot = ready(&control, &accepted).await;
    let path = root.path().join("evidence.txt");
    std::fs::write(&path, b"file fixture").unwrap();
    // Exact ordering of the production boundary: no timer or scheduling race.
    let late = add_human_attachment(&control, &accepted, path).await;
    assert_eq!(
        control
            .attachments
            .prepare_at_revision(
                &accepted.thread_id,
                CancellationToken::new(),
                false,
                snapshot.selection
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    let refreshed = ready(&control, &accepted).await;
    let mut prepared = control
        .attachments
        .prepare_at_revision(
            &accepted.thread_id,
            CancellationToken::new(),
            false,
            refreshed.selection,
        )
        .await
        .unwrap();
    let mut input = Vec::new();
    super::super::attachment_input::bind(
        refreshed.input.as_ref().unwrap(),
        &mut prepared,
        &mut input,
        &mut Vec::new(),
    )
    .unwrap();
    let manifest = prepared.manifest.clone();
    let attempt = id();
    assert!(
        admit_attempt(&control, &refreshed, &attempt, Arc::new(prepared), 0, 0)
            .await
            .unwrap()
    );
    let request = model::response_request("fixture", input, None, false);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        let body = loop {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(at) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..at]);
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= at + 4 + length {
                    break serde_json::from_slice::<Value>(&bytes[at + 4..at + 4 + length])
                        .unwrap();
                }
            }
        };
        let response = json!({"id":"fixture","status":"completed","output":[]}).to_string();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
        body
    });
    let client = HttpModelClient::new(control.store.config.connections["fixture"].clone()).unwrap();
    crate::test_watchdog::watchdog("src/control/driver/preparation_tests.rs: human_acceptance_between_next_and_prepare_refreshes_source_before_http",
        client.response(request, CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap();
    let sent = server.await.unwrap();
    let messages = sent["input"].as_array().unwrap();
    let first = messages
        .iter()
        .position(|item| item["content"].to_string().contains("original instruction"))
        .unwrap();
    let second = messages
        .iter()
        .position(|item| {
            item["content"]
                .to_string()
                .contains("late instruction with attachment")
        })
        .unwrap();
    assert!(first < second);
    assert_eq!(manifest.len(), 1);
    let source = late["message_id"].as_str().unwrap().to_owned();
    assert_eq!(manifest[0].source_node_id, source);
    assert_eq!(manifest[0].api_item_index, Some(second));
    assert_eq!(manifest[0].source_element_index, 1);
    assert!(
        !messages[first]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "input_file")
    );
    assert_eq!(
        messages
            .iter()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .filter(|part| part["type"] == "input_file")
            .count(),
        1
    );
    assert!(
        messages[second]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "input_file")
    );
    control
        .store
        .database
        .call(move |db| {
            let saved: String = db.query_row(
                "SELECT input_manifest_json FROM model_attempt WHERE id=?1",
                [attempt],
                |r| r.get(0),
            )?;
            assert_eq!(
                serde_json::from_str::<Value>(&saved).unwrap()["attachments"][0]["source_node_id"],
                source
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM receipt WHERE state='accepted'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                2,
                "accepted user data is preserved until completion"
            );
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "running"
            );
            Ok(())
        })
        .await
        .unwrap();
    drop(snapshot);
    drop(refreshed);
    close(control, owner).await;
}

#[tokio::test]
async fn permanent_adoption_error_is_typed_recovers_and_leaves_no_partial_attempt() {
    let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let path = root.path().join("evidence.txt");
    std::fs::write(&path, b"file fixture").unwrap();
    add_human_attachment(&control, &accepted, path).await;
    let snapshot = ready(&control, &accepted).await;
    let prepared = control
        .attachments
        .prepare_at_revision(
            &accepted.thread_id,
            CancellationToken::new(),
            false,
            snapshot.selection,
        )
        .await
        .unwrap();
    // Intentionally skip binding: this permanent contract violation used to
    // be flattened to false and cause the driver to repeat preparation.
    assert_eq!(
        admit_attempt(
            &control,
            &snapshot,
            "invalid-unbound",
            Arc::new(prepared),
            0,
            0
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidState
    );
    assert!(matches!(
        next(
            &control,
            &accepted.thread_id,
            accepted.run_id.as_deref().unwrap()
        )
        .await
        .unwrap(),
        Next::Done
    ));
    control
        .store
        .database
        .call(|db| {
            let (turn, generation): (String, i64) =
                db.query_row("SELECT id,generation FROM turn", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            super::recovery_notification_tests::assert_reason_and_event(db, &turn, generation);
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "recovery_wait"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM receipt WHERE state='accepted'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    drop(snapshot);
    close(control, owner).await;
}

#[tokio::test]
async fn client_initialization_failure_recovers_once_instead_of_rekicking_preparation() {
    let (root, mut owner, old_control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    drop(old_control);
    // A fresh synthetic owner registry is needed: Control clones share its live registry.
    owner.loaded.config.live_connections = Default::default();
    // Loaded production config is validated; inject a constructor failure
    // without changing a credential, process-global env, or live owner.
    owner
        .loaded
        .config
        .connections
        .get_mut("fixture")
        .unwrap()
        .base_url = "invalid fixture URL".into();
    let control = Control::new(&owner, root.path().into());
    let slot = DriverSlot {
        id: id(),
        run_id: accepted.run_id.clone().unwrap(),
        stop: CancellationToken::new(),
        nudge: Arc::new(Notify::new()),
        foreground: Arc::new(ForegroundSlot::default()),
        turn: Arc::new(tokio::sync::Mutex::new(None)),

        driver_active: Arc::new(AtomicBool::new(true)),
        proactive_job: Arc::default(),
        proactive_join_done: Arc::new(tokio::sync::Notify::new()),
    };
    assert_eq!(
        crate::test_watchdog::watchdog("src/control/driver/preparation_tests.rs: client_initialization_failure_recovers_once_instead_of_rekicking_preparation",
            drive(control.clone(), accepted.thread_id.clone(), slot.clone())
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        ErrorCode::ConfigInvalid
    );
    for _ in 0..3 {
        crate::test_watchdog::watchdog("src/control/driver/preparation_tests.rs: client_initialization_failure_recovers_once_instead_of_rekicking_preparation",
            drive(control.clone(), accepted.thread_id.clone(), slot.clone()),
        )
        .await
        .unwrap()
        .unwrap();
    }
    control
        .store
        .database
        .call(|db| {
            let (turn, generation): (String, i64) =
                db.query_row("SELECT id,generation FROM turn", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            super::recovery_notification_tests::assert_reason_and_event(db, &turn, generation);
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "recovery_wait"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM instruction_snapshot", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    close(control, owner).await;
}

#[tokio::test]
async fn held_source_is_deferred_but_malformed_or_unexplained_source_still_fails() {
    let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let path = root.path().join("held.txt");
    std::fs::write(&path, b"file fixture").unwrap();
    let late = add_human_attachment(&control, &accepted, path).await;
    let source = late["message_id"].as_str().unwrap().to_owned();
    let original=control.store.database.call({let source=source.clone();move|db|{
        db.execute("UPDATE receipt SET state='held',hold_reason='fixture' WHERE message_node_id=?1",[&source])?;
        Ok(db.query_row("SELECT payload_json FROM node WHERE id=?1",[source],|r|r.get::<_,String>(0))?)
    }}).await.unwrap();
    let prepared = control
        .attachments
        .prepare(&accepted.thread_id, CancellationToken::new(), false)
        .await
        .unwrap();
    assert!(
        prepared.manifest.is_empty(),
        "held source must not move to current request"
    );
    control
        .store
        .database
        .call({
            let source = source.clone();
            let original = original.clone();
            move |db| {
                let mut payload: Value = serde_json::from_str(&original).unwrap();
                let part = payload["content"][1].clone();
                payload["content"].as_array_mut().unwrap().push(part);
                db.execute(
                    "UPDATE node SET payload_json=?2 WHERE id=?1",
                    params![source, payload.to_string()],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let error = control
        .attachments
        .prepare(&accepted.thread_id, CancellationToken::new(), false)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(
        error
            .message
            .contains("exactly one ordered source reference")
    );
    control
        .store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE node SET payload_json=?2 WHERE id=?1",
                params![source, original],
            )?;
            db.execute(
                "UPDATE receipt SET state='applied' WHERE message_node_id=?1",
                [&source],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = control
        .attachments
        .prepare(&accepted.thread_id, CancellationToken::new(), false)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("outside the selected path"));
    drop(snapshot);
    drop(prepared);
    close(control, owner).await;
}

#[tokio::test]
async fn adoption_selection_conflict_rolls_back_without_entering_recovery() {
    let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let path = root.path().join("cas.txt");
    std::fs::write(&path, b"file fixture").unwrap();
    add_human_attachment(&control, &accepted, path).await;
    let mut snapshot = ready(&control, &accepted).await;
    let mut prepared = control
        .attachments
        .prepare_at_revision(
            &accepted.thread_id,
            CancellationToken::new(),
            false,
            snapshot.selection,
        )
        .await
        .unwrap();
    let mut input = Vec::new();
    super::super::attachment_input::bind(
        snapshot.input.as_ref().unwrap(),
        &mut prepared,
        &mut input,
        &mut Vec::new(),
    )
    .unwrap();
    let thread = accepted.thread_id.clone();
    control
        .store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE thread SET selection_revision=selection_revision+1 WHERE id=?1",
                [thread],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    snapshot.selection += 1;
    assert_eq!(
        admit_attempt(
            &control,
            &snapshot,
            "cas-conflict",
            Arc::new(prepared),
            0,
            0
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::OperationConflict
    );
    assert!(matches!(
        next(
            &control,
            &accepted.thread_id,
            accepted.run_id.as_deref().unwrap()
        )
        .await
        .unwrap(),
        Next::Ready(_)
    ));
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "preparing"
            );
            Ok(())
        })
        .await
        .unwrap();
    drop(snapshot);
    close(control, owner).await;
}

#[tokio::test]
async fn completed_attempt_cannot_adopt_its_tool_group_a_second_time() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let cancelled = control
        .store
        .execute(
            Request::new(
                format!("{}:cancelled-input", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(accepted.thread_id.clone()),
                    text: "cancel this before admission".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cancelled = crate::protocol::WaitCondition::MessageDelivered {
        message_id: cancelled,
    };
    let cancellation_wait = tokio::spawn(super::super::wait::observe(
        control.clone(),
        "parked-before-cancellation".into(),
        crate::protocol::WaitArgs {
            conditions: vec![cancelled.clone()],
            timeout_ms: 30_000,
            after: None,
        },
        None,
        CancellationToken::new(),
    ));
    crate::test_watchdog::watchdog(
        "delivery wait parks before cancellation",
        control.waits.parked.notified(),
    )
    .await
    .unwrap();
    let crate::protocol::WaitCondition::MessageDelivered { message_id } = &cancelled else {
        unreachable!()
    };
    let cancellation = control
        .request(Request::new(
            format!("{}:cancel-input", owner.instance_id),
            Command::MessageCancel(crate::protocol::MessageCancelArgs {
                message_ids: vec![message_id.clone()],
            }),
        ))
        .await
        .result
        .unwrap();
    assert_eq!(cancellation["items"][0]["outcome"], "cancelled");
    let cancelled_result = crate::test_watchdog::watchdog(
        "cancellation commit wakes exact delivery wait",
        cancellation_wait,
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(cancelled_result["status"], "unavailable");
    assert_eq!(cancelled_result["matched"], json!([]));
    let snapshot = ready(&control, &accepted).await;
    let prepared = Arc::new(
        control
            .attachments
            .prepare_at_revision(
                &accepted.thread_id,
                CancellationToken::new(),
                false,
                snapshot.selection,
            )
            .await
            .unwrap(),
    );
    assert!(
        admit_attempt(
            &control,
            &snapshot,
            "deduplicated-attempt",
            prepared.clone(),
            0,
            0
        )
        .await
        .unwrap()
    );
    let generating = control
        .request(Request::new(
            format!("{}:generating-hold", owner.instance_id),
            Command::MessageGet(crate::protocol::MessageTarget {
                message_id: accepted.message_id.clone(),
            }),
        ))
        .await
        .result
        .unwrap();
    assert_eq!(generating["held_by"], json!({"kind":"response_generation"}));
    let condition = crate::protocol::WaitCondition::MessageDelivered {
        message_id: accepted.message_id.clone(),
    };
    let pending = super::super::wait::observe(
        control.clone(),
        "before-application".into(),
        crate::protocol::WaitArgs {
            conditions: vec![condition.clone()],
            timeout_ms: 0,
            after: None,
        },
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(pending["status"], "timeout");
    assert_eq!(pending["matched"], json!([]));
    let observer = tokio::spawn(super::super::wait::observe(
        control.clone(),
        "parked-before-application".into(),
        crate::protocol::WaitArgs {
            conditions: vec![condition.clone()],
            timeout_ms: 30_000,
            after: None,
        },
        None,
        CancellationToken::new(),
    ));
    crate::test_watchdog::watchdog(
        "message application wait registered",
        control.waits.parked.notified(),
    )
    .await
    .unwrap();
    let later = control
        .store
        .execute(
            Request::new(
                format!("{}:later-input", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(accepted.thread_id.clone()),
                    text: "not included in the admitted attempt".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let later_id = later["message_id"].as_str().unwrap().to_owned();
    let disconnect = CancellationToken::new();
    let unrelated = tokio::spawn(super::super::wait::observe(
        control.clone(),
        "parked-for-later-input".into(),
        crate::protocol::WaitArgs {
            conditions: vec![crate::protocol::WaitCondition::MessageDelivered {
                message_id: later_id.clone(),
            }],
            timeout_ms: 30_000,
            after: None,
        },
        None,
        disconnect.clone(),
    ));
    crate::test_watchdog::watchdog(
        "later message wait initially parked",
        control.waits.parked.notified(),
    )
    .await
    .unwrap();
    let response = model::ModelResponse::completed(json!({"id":"response","status":"completed","output":[{"type":"function_call","name":"shell","call_id":"call","arguments":"{\"command\":\"printf unused\"}"}]}),true).unwrap();
    model::request_path::persist_received_attempt(
        &control.store.database,
        "deduplicated-attempt",
        model::request_path::AttemptKind::Conversation,
        false,
        &Ok(response.clone()),
    )
    .await
    .unwrap();
    assert!(
        adopt_response(
            &control,
            &snapshot,
            "deduplicated-attempt",
            &response,
            prepared.clone(),
            Some("first-group".into()),
            Vec::new(),
        )
        .await
        .unwrap()
    );
    let observed = crate::test_watchdog::watchdog(
        "message application commit wakes the registered wait",
        observer,
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(observed["matched"], json!([condition]));
    crate::test_watchdog::watchdog(
        "unrelated adoption wakes but does not satisfy later message wait",
        control.waits.parked.notified(),
    )
    .await
    .unwrap();
    assert!(!unrelated.is_finished());
    disconnect.cancel();
    let unrelated =
        crate::test_watchdog::watchdog("later message observer ends on disconnect", unrelated)
            .await
            .unwrap()
            .unwrap();
    if let Ok(result) = unrelated {
        assert_eq!(result["matched"], json!([]));
    }
    let cancelled_only = super::super::wait::observe(
        control.clone(),
        "cancelled-delivery".into(),
        crate::protocol::WaitArgs {
            conditions: vec![cancelled.clone()],
            timeout_ms: 0,
            after: None,
        },
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(cancelled_only["status"], "unavailable");
    assert_eq!(cancelled_only["matched"], json!([]));
    let mixed = super::super::wait::observe(
        control.clone(),
        "mixed-application".into(),
        crate::protocol::WaitArgs {
            conditions: vec![cancelled, condition.clone()],
            timeout_ms: 0,
            after: None,
        },
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(mixed["status"], "satisfied");
    assert_eq!(mixed["matched"], json!([condition]));
    let applied = super::super::wait::observe(
        control.clone(),
        "after-application".into(),
        crate::protocol::WaitArgs {
            conditions: vec![condition.clone()],
            timeout_ms: 0,
            after: None,
        },
        None,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(applied["status"], "satisfied");
    assert_eq!(applied["matched"], json!([condition]));
    assert!(
        !adopt_response(
            &control,
            &snapshot,
            "deduplicated-attempt",
            &response,
            prepared.clone(),
            Some("duplicate-group".into()),
            Vec::new(),
        )
        .await
        .unwrap()
    );
    control
        .store
        .database
        .call(|db| {
            for query in [
                "SELECT count(*) FROM node WHERE kind='assistant_response'",
                "SELECT count(*) FROM output_item",
                "SELECT count(*) FROM tool_group",
                "SELECT count(*) FROM receipt WHERE state='applied'",
            ] {
                assert_eq!(db.query_row(query, [], |r| r.get::<_, i64>(0))?, 1);
            }
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "running"
            );
            Ok(())
        })
        .await
        .unwrap();
    drop(prepared);
    drop(snapshot);
    close(control, owner).await;
}

#[tokio::test]
async fn obsolete_recovery_leaves_history_unchanged_and_keeps_attempt_audit() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let prepared = Arc::new(
        control
            .attachments
            .prepare_at_revision(
                &accepted.thread_id,
                CancellationToken::new(),
                false,
                snapshot.selection,
            )
            .await
            .unwrap(),
    );
    assert!(
        admit_attempt(
            &control,
            &snapshot,
            "late-cancelled",
            prepared.clone(),
            0,
            0
        )
        .await
        .unwrap()
    );
    let before = control
        .store
        .database
        .call(|db| {
            let tx = db.transaction()?;
            tx.execute("UPDATE run SET state='stopped'", [])?;
            tx.execute(
                "UPDATE turn SET state='stopped',generation=generation+1",
                [],
            )?;
            let revision = tx.query_row(
                "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'",
                [],
                |r| r.get::<_, i64>(0),
            )?;
            let path = tx
                .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            tx.commit()?;
            Ok((revision, path))
        })
        .await
        .unwrap();
    let mut failure: model::http::AttemptFailure =
        model::ModelError::new("late", "late failure").into();
    failure.partial_text = "saved partial".into();
    failure.raw_response = Some(json!({"fixture":"raw bytes represented as JSON"}));
    model::request_path::persist_received_attempt(
        &control.store.database,
        "late-cancelled",
        model::request_path::AttemptKind::Conversation,
        true,
        &Err(failure.clone()),
    )
    .await
    .unwrap();
    recover(
        &control,
        &snapshot,
        ApiError::new(ErrorCode::IoFailed, "late failure"),
        Some("late-cancelled".into()),
    )
    .await
    .unwrap();
    control
        .store
        .database
        .call(move |db| {
            let saved = db.query_row(
                "SELECT state,ended_at,error_json FROM model_attempt WHERE id='late-cancelled'",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )?;
            assert_eq!(saved.0, "cancelled");
            assert!(!saved.1.is_empty());
            let audit: Value = serde_json::from_str(&saved.2.unwrap()).unwrap();
            assert_eq!(audit["partial_text"], "saved partial");
            assert_eq!(
                audit["raw_response"],
                json!({"fixture":"raw bytes represented as JSON"})
            );
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "stopped"
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM node WHERE kind='incomplete_response'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                before.0
            );
            let path = db
                .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(path, before.1);
            Ok(())
        })
        .await
        .unwrap();
    drop(prepared);
    drop(snapshot);
    close(control, owner).await;
}

#[tokio::test]
async fn saved_tool_result_notifies_its_selected_history_only_after_commit() {
    for rollback in [false, true] {
        let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let snapshot = ready(&control, &accepted).await;
        let prepared = Arc::new(
            control
                .attachments
                .prepare_at_revision(
                    &accepted.thread_id,
                    CancellationToken::new(),
                    false,
                    snapshot.selection,
                )
                .await
                .unwrap(),
        );
        assert!(
            admit_attempt(
                &control,
                &snapshot,
                "result-attempt",
                prepared.clone(),
                0,
                0
            )
            .await
            .unwrap()
        );
        let response = model::ModelResponse::completed(
            json!({
                "id":"result-response", "status":"completed", "output":[{
                    "type":"function_call", "name":"shell", "call_id":"result-call",
                    "arguments":"{\"command\":\"unused synthetic call\"}",
                }],
            }),
            true,
        )
        .unwrap();
        model::request_path::persist_received_attempt(
            &control.store.database,
            "result-attempt",
            model::request_path::AttemptKind::Conversation,
            false,
            &Ok(response.clone()),
        )
        .await
        .unwrap();
        assert!(
            adopt_response(
                &control,
                &snapshot,
                "result-attempt",
                &response,
                prepared,
                Some("result-group".into()),
                Vec::new(),
            )
            .await
            .unwrap()
        );
        if rollback {
            control.store.database.call(|db| {
                db.execute_batch("CREATE TRIGGER reject_result BEFORE INSERT ON tool_result BEGIN SELECT RAISE(ABORT,'fixture result rollback'); END;")?;
                Ok(())
            }).await.unwrap();
        }
        // The adopted call's notification has already happened. Only the saved
        // result can make this subscription observe another history change.
        let mut subscription = control.subscribe_ui();
        let result = save_tool_result(
            &control,
            &snapshot,
            "result-group",
            "result-call",
            json!({"status":"completed","text":"COMMITTED_TOOL_RESULT"}),
        )
        .await;
        if rollback {
            assert!(result.is_err());
            assert!(matches!(
                subscription.receiver.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
        } else {
            result.unwrap();
            assert!(
                matches!(subscription.receiver.try_recv(), Ok(EngineEvent::Changed {thread_id,recovery:false}) if thread_id==accepted.thread_id)
            );
            assert!(matches!(
                subscription.receiver.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
        }
        let thread = accepted.thread_id.clone();
        control.store.database.call(move |db| {
            let payloads = db.prepare("SELECT n.payload_json FROM tool_result r JOIN node n ON n.id=r.node_id JOIN thread_path p ON p.node_id=n.id WHERE p.thread_id=?1 AND r.group_id='result-group' AND r.call_id='result-call'")?
                .query_map([thread], |row| row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(payloads.len(), usize::from(!rollback));
            assert_eq!(db.query_row("SELECT COUNT(*) FROM node WHERE kind='tool_result'",[],|row|row.get::<_,i64>(0))?, i64::from(!rollback));
            if !rollback { assert!(payloads[0].contains("COMMITTED_TOOL_RESULT")); }
            assert_eq!(db.query_row("SELECT state FROM tool_call WHERE group_id='result-group' AND call_id='result-call'",[],|row|row.get::<_,String>(0))?, if rollback {"pending"} else {"completed"});
            assert_eq!(db.query_row("SELECT COUNT(*) FROM model_attempt",[],|row|row.get::<_,i64>(0))?,1);
            if rollback { db.execute_batch("DROP TRIGGER reject_result")?; }
            Ok(())
        }).await.unwrap();
        drop(snapshot);
        drop(subscription);
        close(control, owner).await;
    }
}

#[tokio::test]
async fn already_admitted_tool_calls_cannot_create_an_execution_or_spawn_again() {
    for saved in ["running", "completed"] {
        let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let snapshot = ready(&control, &accepted).await;
        let prepared = Arc::new(
            control
                .attachments
                .prepare_at_revision(
                    &accepted.thread_id,
                    CancellationToken::new(),
                    false,
                    snapshot.selection,
                )
                .await
                .unwrap(),
        );
        assert!(
            admit_attempt(&control, &snapshot, "tool-attempt", prepared.clone(), 0, 0)
                .await
                .unwrap()
        );
        let response=model::ModelResponse::completed(json!({"id":"response","status":"completed","output":[{"type":"function_call","name":"shell","call_id":"call","arguments":"{\"command\":\"printf unused\"}"}]}),true).unwrap();
        model::request_path::persist_received_attempt(
            &control.store.database,
            "tool-attempt",
            model::request_path::AttemptKind::Conversation,
            false,
            &Ok(response.clone()),
        )
        .await
        .unwrap();
        assert!(
            adopt_response(
                &control,
                &snapshot,
                "tool-attempt",
                &response,
                prepared.clone(),
                Some("group".into()),
                Vec::new(),
            )
            .await
            .unwrap()
        );
        if saved == "completed" {
            save_tool_result(
                &control,
                &snapshot,
                "group",
                "call",
                json!({"status":"completed","original_result":"ORIGINAL_RESULT"}),
            )
            .await
            .unwrap();
        }
        let state = saved.to_owned();
        control
            .store
            .database
            .call(move |db| {
                db.execute(
                    "UPDATE tool_call SET state=?1,operation_id='original-operation'",
                    [state],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let marker = root.path().join("must-not-spawn");
        let args: ShellToolArgs=serde_json::from_value(json!({"command":format!("printf duplicate > '{}'",marker.display()),"background":true})).unwrap();
        let slot = DriverSlot {
            id: id(),
            run_id: accepted.run_id.clone().unwrap(),
            stop: CancellationToken::new(),
            nudge: Arc::new(Notify::new()),
            foreground: Arc::new(ForegroundSlot::default()),
            turn: Arc::new(tokio::sync::Mutex::new(None)),

            driver_active: Arc::new(AtomicBool::new(true)),
            proactive_job: Arc::default(),
            proactive_join_done: Arc::new(tokio::sync::Notify::new()),
        };
        assert_eq!(
            run_shell(
                control.clone(),
                &snapshot,
                &slot,
                "group",
                "call",
                args.clone()
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::OperationConflict
        );
        assert_eq!(
            run_tool_group(
                &control,
                &snapshot,
                &slot,
                "group",
                vec![model::ToolCall {
                    call_id: "call".into(),
                    item_id: None,
                    args: Ok(args),
                    unknown_arguments: Vec::new(),
                }]
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::OperationConflict
        );
        assert!(!marker.exists());
        assert!(
            !control
                .storage_failed
                .load(std::sync::atomic::Ordering::Acquire),
            "an existing completed result must not become a storage failure"
        );
        let expected_state = saved.to_owned();
        control
            .store
            .database
            .call(move |db| {
                assert_eq!(db.query_row("SELECT state FROM tool_call WHERE group_id='group' AND call_id='call'",[],|r|r.get::<_,String>(0))?,expected_state);
                assert_eq!(db.query_row("SELECT state FROM tool_group WHERE id='group'",[],|r|r.get::<_,String>(0))?,"running");
                let results: Vec<String> = db.prepare("SELECT n.payload_json FROM tool_result r JOIN node n ON n.id=r.node_id WHERE r.group_id='group' AND r.call_id='call'")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
                assert_eq!(results.len(),usize::from(expected_state == "completed"));
                assert!(results.iter().all(|result| result.contains("ORIGINAL_RESULT")));
                assert_eq!(db.query_row("SELECT COUNT(*) FROM node WHERE kind='tool_result'",[],|r|r.get::<_,i64>(0))?,results.len() as i64);
                assert_eq!(
                    db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                        .get::<_, i64>(0))?,
                    0,
                    "admission INSERT is rolled back before OS spawn"
                );
                assert_eq!(
                    db.query_row("SELECT operation_id FROM tool_call", [], |r| r
                        .get::<_, String>(0))?,
                    "original-operation"
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(prepared);
        drop(snapshot);
        close(control, owner).await;
    }
}

#[tokio::test]
async fn suppressed_turn_start_is_a_conflict_and_keeps_the_saved_request_ready() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let prepared = Arc::new(
        control
            .attachments
            .prepare_at_revision(
                &accepted.thread_id,
                CancellationToken::new(),
                false,
                snapshot.selection,
            )
            .await
            .unwrap(),
    );
    control.store.database.call(|db| {db.execute_batch("CREATE TRIGGER suppress_turn_start BEFORE UPDATE OF state ON turn WHEN NEW.state='running' BEGIN SELECT RAISE(IGNORE); END;")?;Ok(())}).await.unwrap();
    assert_eq!(
        admit_attempt(&control, &snapshot, "unadmitted", prepared.clone(), 0, 0)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "preparing"
            );
            assert_eq!(
                db.query_row("SELECT state FROM receipt", [], |r| r.get::<_, String>(0))?,
                "accepted"
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        next(
            &control,
            &accepted.thread_id,
            accepted.run_id.as_deref().unwrap()
        )
        .await
        .unwrap(),
        Next::Ready(_)
    ));
    drop(prepared);
    drop(snapshot);
    close(control, owner).await;
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn interrupted_reserved_call_finishes_its_pair_without_native_creation() {
    let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let slot = DriverSlot {
        id: id(),
        run_id: accepted.run_id.clone().unwrap(),
        stop: CancellationToken::new(),
        nudge: Arc::new(Notify::new()),
        foreground: Arc::new(ForegroundSlot::default()),
        turn: Arc::new(Mutex::new(None)),

        driver_active: Arc::new(AtomicBool::new(true)),
        proactive_job: Arc::default(),
        proactive_join_done: Arc::new(tokio::sync::Notify::new()),
    };
    let binding = slot.bind_turn(&control, &snapshot.turn).await.unwrap();
    let mut gate = binding.start_gate.lock().await;
    let turn = snapshot.turn.clone();
    control.store.database.call(move|db|{
        db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?1),'gate-attempt',?1,0,0,'fixture','fixture',0,'completed','{}','now')",[&turn])?;
        db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('gate-group','gate-attempt',?1,'running','now')",[turn])?;
        db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('gate-group','gate-call',0,'shell','{}','pending')",[])?;Ok(())
    }).await.unwrap();
    let task_control = control.clone();
    let task_slot = slot.clone();
    let task = tokio::spawn(async move {
        let args:ShellToolArgs=serde_json::from_value(json!({"command":if cfg!(windows){"[IO.File]::WriteAllText('must-not-exist','forbidden')"}else{"printf forbidden > must-not-exist"}})).unwrap();
        run_tool_group(
            &task_control,
            &snapshot,
            &task_slot,
            "gate-group",
            vec![model::ToolCall {
                call_id: "gate-call".into(),
                item_id: None,
                args: Ok(args),
                unknown_arguments: Vec::new(),
            }],
        )
        .await
    });
    crate::test_watchdog::watchdog("src/control/driver/preparation_tests.rs: interrupted_reserved_call_finishes_its_pair_without_native_creation", async {
        loop {
            let reserved = control
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM shell_execution WHERE state='starting')",
                        [],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if reserved {
                break;
            }
            // Reservation precedes native creation and has no guaranteed
            // Changed notification while the native-start gate is held.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Deterministic synthetic interrupt commit while native creation is fenced.
    let turn = accepted.turn_id.unwrap();
    control.store.database.call(move|db|{db.execute("UPDATE turn SET interrupt_requested_at='fixture',generation=generation+1 WHERE id=?1",[turn])?;Ok(())}).await.unwrap();
    *gate = true;
    binding.cancel.cancel();
    drop(gate);
    crate::test_watchdog::watchdog("src/control/driver/preparation_tests.rs: interrupted_reserved_call_finishes_its_pair_without_native_creation", task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    control.store.database.call(|db|{
        assert_eq!(db.query_row("SELECT state,end_reason FROM shell_execution",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?,("stopped".into(),"stopped_before_spawn".into()));
        assert_eq!(db.query_row("SELECT state FROM tool_group WHERE id='gate-group'",[],|r|r.get::<_,String>(0))?,"completed");
        let body:String=db.query_row("SELECT n.payload_json FROM tool_result r JOIN node n ON n.id=r.node_id WHERE r.group_id='gate-group'",[],|r|r.get(0))?;
        let body:Value=serde_json::from_str(&body).unwrap();
        let result:Value=serde_json::from_str(body["item"]["output"].as_str().unwrap()).unwrap();
        assert_eq!(result["executed"],false);assert_eq!(result["reason"],"human_turn_interrupted");
        Ok(())
    }).await.unwrap();
    assert!(control.origins.lock().await.is_empty());
    assert!(!root.path().join("must-not-exist").exists());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_unfinished_group_waits_for_pairs_instead_of_rechecking() {
    for state in ["running", "recovery_wait"] {
        let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let snapshot = ready(&control, &accepted).await;
        let saved_turn = snapshot.turn.clone();
        control.store.database.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?1),'orphan-attempt',?1,0,0,'fixture','fixture',0,'completed','{}','now')", [&saved_turn])?;
            tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('orphan-group','orphan-attempt',?1,'running','now')", [&saved_turn])?;
            tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('orphan-group','orphan-call',0,'shell','{}','running')", [])?;
            tx.execute("UPDATE turn SET state=?2 WHERE id=?1", params![saved_turn,state])?;
            tx.commit()?; Ok(())
        }).await.unwrap();
        let result = super::super::turn_interrupt::admit_locked(
            &control,
            &snapshot.thread,
            &snapshot.turn,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result["requested"], true);
        super::super::turn_interrupt::acknowledge(&control, &snapshot.thread, &snapshot.run)
            .await
            .unwrap();
        assert!(
            matches!(
                next(&control, &snapshot.thread, &snapshot.run)
                    .await
                    .unwrap(),
                Next::Idle
            ),
            "an unfinished group must wait for a nudge or stop, including recovery_wait"
        );
        let saved_turn = snapshot.turn.clone();
        control
            .store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row("SELECT state FROM turn WHERE id=?1", [saved_turn], |r| {
                        r.get::<_, String>(0)
                    })?,
                    state
                );
                assert_eq!(
                    db.query_row(
                        "SELECT state FROM tool_group WHERE id='orphan-group'",
                        [],
                        |r| r.get::<_, String>(0)
                    )?,
                    "running"
                );
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM tool_result", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r
                        .get::<_, i64>(0))?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
        // Existing full Run stop must remain selectable while that pair is
        // unresolved. Do not invent a successful tool result to unblock it.
        let slot = DriverSlot {
            id: id(),
            run_id: snapshot.run.clone(),
            stop: CancellationToken::new(),
            nudge: Arc::new(Notify::new()),
            foreground: Arc::new(ForegroundSlot::default()),
            turn: Arc::new(Mutex::new(None)),

            driver_active: Arc::new(AtomicBool::new(true)),
            proactive_job: Arc::default(),
            proactive_join_done: Arc::new(tokio::sync::Notify::new()),
        };
        let stop = slot.stop.clone();
        let running = tokio::spawn(drive(control.clone(), snapshot.thread.clone(), slot));
        stop.cancel();
        crate::test_watchdog::watchdog("src/control/driver/preparation_tests.rs: interrupted_unfinished_group_waits_for_pairs_instead_of_rechecking", running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let saved_run = snapshot.run.clone();
        control
            .store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row("SELECT state FROM run WHERE id=?1", [saved_run], |r| r
                        .get::<_, String>(
                        0
                    ))?,
                    "stopped"
                );
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r
                        .get::<_, i64>(0))?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(snapshot);
        close(control, owner).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_reported_running_shell_can_be_backgrounded_immediately() {
    crate::test_watchdog::watchdog("running shell foreground publication", async {
        for wire in [false, true] {
        let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let snapshot = ready(&control, &accepted).await;
        let turn = snapshot.turn.clone();
        control.store.database.call(move |db| {
            // The fixture publishes an adopted tool group without a model dispatch.
            db.execute("UPDATE turn SET state='running' WHERE id=?1", [&turn])?;
            db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?1),'publication-attempt',?1,0,0,'fixture','fixture',0,'completed','{}','now')", [&turn])?;
            db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('publication-group','publication-attempt',?1,'running','now')", [turn])?;
            db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('publication-group','publication-call',0,'shell','{}','pending')", [])?;
            db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('publication-group','other-call',1,'shell','{}','pending')", [])?;
            Ok(())
            }).await.unwrap();
        let slot = control.live.register_or_nudge_driver(
            &snapshot.thread, snapshot.run.clone(), &control.stop,
        ).await.unwrap();
        let other_args = serde_json::from_value(json!({"command":"cat","stdin_open":true,"background":true})).unwrap();
        let (other, _) = run_shell(control.clone(), &snapshot, &slot, "publication-group", "other-call", other_args).await.unwrap();
        assert_eq!(other["status"], "running");
        // Poll the real driver only at explicit steps. The DB can commit its
        // running row while the driver's response future has not resumed.
        let args = serde_json::from_value(json!({"command":"cat","stdin_open":true})).unwrap();
        let shell = run_shell(control.clone(), &snapshot, &slot,
            "publication-group", "publication-call", args);
        tokio::pin!(shell);
        let execution = loop {
            assert!(futures_util::poll!(shell.as_mut()).is_pending());
            let running = control.store.database.call(|db| {
                use rusqlite::OptionalExtension;
                Ok(db.query_row("SELECT s.id FROM shell_execution s JOIN tool_call c ON c.operation_id=s.id WHERE s.state='running' AND c.call_id='publication-call'", [], |row| row.get::<_, String>(0)).optional()?)
                }).await.unwrap();
            if let Some(id) = running { break id; }
            tokio::task::yield_now().await;
        };
        assert_ne!(other["execution_id"], execution);
        let send = |suffix: &str, deliver| Request::new(format!("{}:{suffix}", owner.instance_id),
            Command::MessageSend(MessageSendArgs { max_model_requests: None, resume_recovery: false,
                human: false, sender_label: None, deliver, thread_id: Some(snapshot.thread.clone()), text: "fresh queued input".into() }));
        let earlier = control.request(send("earlier", crate::protocol::Deliver::Next)).await.result.unwrap();
        let named = control.store.execute(Request::new(
            format!("{}:named-between-inputs", owner.instance_id),
            Command::MessageAsk(crate::protocol::MessageAskArgs {
                to: Some("human".into()), human: false, sender_label: None,
                deliver: crate::protocol::Deliver::Next, thread_id: None,
                text: "synthetic decision".into(),
            }),
        ), Some(crate::protocol::Origin {
            instance_id: owner.instance_id.clone(), thread_id: snapshot.thread.clone(),
            run_id: snapshot.run.clone(), turn_id: snapshot.turn.clone(), execution_id: execution.clone(),
            token: String::new(),
        })).await.unwrap().result;
        assert_ne!(earlier["message_id"], named["message_id"]);
        let pending = control.request(send("pending", crate::protocol::Deliver::Next)).await.result.unwrap();
        let revisit = control.request(send("revisit", crate::protocol::Deliver::Complete)).await.result.unwrap();
        let get = |value: &Value| Request::new(format!("{}:hold", owner.instance_id),
            Command::MessageGet(crate::protocol::MessageTarget { message_id: value["message_id"].as_str().unwrap().into() }));
        let message_get = get(&pending);
        let detail = control.request(message_get.clone()).await.result.unwrap();
        assert_eq!(detail["held_by"], json!({"kind":"foreground_shell","execution_id":execution}));
        let detail = control.request(get(&revisit)).await.result.unwrap();
        assert_eq!(detail["held_by"], json!({"kind":"run_completion"}));
        {
            let _busy = control.live.test_drivers().lock().await;
            let detail = control.request(message_get.clone()).await.result.unwrap();
            assert_eq!(detail["held_by"], json!({"kind":"unknown"}));
        }
        let accepted = if wire {
            let wrong = control.request(Request::new(format!("{}:stale", owner.instance_id), Command::ShellBackground(crate::protocol::ExecutionTarget { execution_id: "stale".into() }))).await;
            assert_eq!(wrong.error.unwrap().details.unwrap()["reason"], "execution_not_found");
            let request = Request::new(format!("{}:background", owner.instance_id), Command::ShellBackground(crate::protocol::ExecutionTarget { execution_id: execution.clone() }));
            let response = control.request(request.clone()).await;
            let replayed = control.request(request).await;
            assert_eq!(replayed.result, response.result);
            response.result.ok_or_else(|| response.error.unwrap())
        } else {
            control.background(&snapshot.thread, None).await
        };
        if let Ok(ack) = &accepted { assert_eq!(ack, &json!({"execution_id":execution,"requested":true})); }
        let first_admitted = accepted.is_ok();
        if !first_admitted {
            // Reap the deliberately exposed old boundary before reporting red.
            assert!(futures_util::poll!(shell.as_mut()).is_pending());
            control.background(&snapshot.thread, None).await.unwrap();
        }
        let (result, halt) = shell.await.unwrap();
        let detail = control.request(message_get).await.result.unwrap();
        assert!(detail.get("held_by").is_none());
        assert!(halt);
        assert_eq!(result, json!({"status":"running","execution_id":execution,"reason":"shell_backgrounded","cancel_remaining_calls":true}));
        save_tool_result(&control, &snapshot, "publication-group", "publication-call", result).await.unwrap();
        control.store.database.call(move |db| {
            let (body, shell): (String, String) = db.query_row("SELECT json_extract(n.payload_json,'$.item.output'),s.state FROM tool_result r JOIN node n ON n.id=r.node_id JOIN shell_execution s ON s.id=?1 WHERE r.group_id='publication-group' AND r.call_id='publication-call'", [&execution], |row| Ok((row.get(0)?,row.get(1)?)))?;
            let saved: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(saved["reason"], "shell_backgrounded");
            assert_eq!(saved["status"], "running");
            assert_eq!(saved["cancel_remaining_calls"], true);
            assert_eq!(shell, "running");
            Ok(())
        }).await.unwrap();
        control.live.remove_driver_if_current(&snapshot.thread, &slot.id).await;
        close(control, owner).await;
        assert!(first_admitted, "running was published before foreground admission: {accepted:?}");
        }
    }).await.unwrap();
}

#[tokio::test]
async fn successful_attempt_admission_notifies_after_preparing_browser_read_without_headers() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let target = snapshot.turn.clone();
    let before: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row("SELECT state FROM turn WHERE id=?1", [target], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(
        before, "preparing",
        "fixed first browser-read boundary before admission"
    );
    let prepared = Arc::new(
        control
            .attachments
            .prepare_at_revision(
                &accepted.thread_id,
                CancellationToken::new(),
                false,
                snapshot.selection,
            )
            .await
            .unwrap(),
    );
    let mut events = control.subscribe();
    assert!(
        admit_attempt(
            &control,
            &snapshot,
            "notified-admission",
            prepared.clone(),
            0,
            0
        )
        .await
        .unwrap()
    );
    let target = snapshot.turn.clone();
    let after:(String,String)=control.store.database.call(move|db|Ok(db.query_row("SELECT u.state,a.state FROM turn u JOIN model_attempt a ON a.turn_id=u.id WHERE u.id=?1",[target],|r|Ok((r.get(0)?,r.get(1)?)))?)).await.unwrap();
    assert_eq!(after, ("running".into(), "inflight".into()));
    // No HTTP is started, so headers, model failure, periodic refresh or a later
    // user event cannot repair a missing committed-state notification.
    let mut changed = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event,EngineEvent::Changed {thread_id,..} if thread_id==accepted.thread_id) {
            changed += 1;
        }
    }
    assert_eq!(
        changed, 1,
        "admission completion must notify observers that already consumed preparing"
    );
    let mut stale = (*snapshot).clone();
    stale.selection += 1;
    assert!(
        !admit_attempt(&control, &stale, "rejected-admission", prepared, 0, 0)
            .await
            .unwrap()
    );
    assert!(
        events.try_recv().is_err(),
        "uncommitted/rejected admission must not mint a change"
    );
    drop(stale);
    drop(snapshot);
    close(control, owner).await;
}

#[tokio::test]
async fn stop_accepted_after_driver_exit_choice_is_settled_at_retirement() {
    crate::test_watchdog::watchdog("stop/driver retirement ownership handoff", async {
        let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let snapshot = ready(&control, &accepted).await;
        let prepared = Arc::new(control.attachments.prepare_at_revision(
            &accepted.thread_id, CancellationToken::new(), false, snapshot.selection,
        ).await.unwrap());
        assert!(admit_attempt(&control, &snapshot, "held-no-http", prepared, 0, 0).await.unwrap());
        let run = accepted.run_id.as_ref().unwrap();
        let slot = control.live.register_or_nudge_driver(
            &accepted.thread_id, run.clone(), &control.stop,
        ).await.unwrap();
        // The driver's outcome is already chosen, but its owned slot still exists.
        let stopped = control.store.execute(Request::new(
            format!("{}:stop", owner.instance_id), Command::RunStop(RunTarget { run_id:run.clone() }),
        ), None).await.unwrap();
        assert_eq!(stopped.stop_runs, vec![run.clone()]);
        assert!(matches!(next(&control, &accepted.thread_id, run).await.unwrap(), Next::Done));
        control.stop_run(run).await;
        assert!(slot.stop.is_cancelled());
        let target = run.clone();
        let before: (String,String) = control.store.database.call(move |db| Ok(db.query_row(
            "SELECT r.state,a.state FROM run r JOIN turn u ON u.run_id=r.id JOIN model_attempt a ON a.turn_id=u.id WHERE r.id=?1",
            [target], |r| Ok((r.get(0)?,r.get(1)?)),
        )?)).await.unwrap();
        assert_eq!(before, ("stopping".into(), "inflight".into()));
        control.retire_driver(accepted.thread_id.clone(), slot).await;
        let target = run.clone();
        control.store.database.call(move |db| {
            let state:String = db.query_row("SELECT state FROM run WHERE id=?1", [&target], |r|r.get(0))?;
            assert_eq!(state, "stopped", "retired driver must not abandon its accepted stop");
            let state:String=db.query_row("SELECT state FROM turn WHERE run_id=?1", [&target], |r|r.get(0))?;
            assert_eq!(state,"stopped");
            assert_eq!(db.query_row("SELECT COUNT(*) FROM model_attempt WHERE state='inflight'", [], |r|r.get::<_,i64>(0))?,0);
            Ok(())
        }).await.unwrap();
        drop(snapshot);
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn stopping_keeps_queued_input_held_with_the_recorded_reason() {
    for explicit in [true, false] {
        let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let run = accepted.run_id.unwrap();
        if explicit {
            control
                .store
                .execute(
                    Request::new(
                        format!("{}:stop", owner.instance_id),
                        Command::RunStop(RunTarget { run_id: run }),
                    ),
                    None,
                )
                .await
                .unwrap();
        } else {
            try_drain_and_stop(&control, &run, None).await.unwrap();
        }
        control
            .store
            .database
            .call(move |db| {
                let (receipt, reason): (String, String) =
                    db.query_row("SELECT state,hold_reason FROM receipt", [], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?;
                assert_eq!(receipt, "held");
                assert_eq!(
                    reason,
                    if explicit {
                        "stopping"
                    } else {
                        "terminal_before_path_admission"
                    }
                );
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_stop_completion_does_not_start_model_work() {
    crate::test_watchdog::watchdog("serialized absent-driver stop handoff", async {
        let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
        let run = accepted.run_id.as_ref().unwrap();
        control.store.execute(Request::new(format!("{}:stop",owner.instance_id),
            Command::RunStop(RunTarget {run_id:run.clone()})), None).await.unwrap();
        let (one,two)=tokio::join!(try_stop_without_driver(&control,run),try_stop_without_driver(&control,run));
        one.unwrap();two.unwrap();
        let target=run.clone();
        let observed:(String,String,i64)=control.store.database.call(move|db| Ok(db.query_row(
            "SELECT r.state,u.state,(SELECT COUNT(*) FROM model_attempt) FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id=?1",
            [target],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)).await.unwrap();
        assert_eq!(observed,("stopped".into(),"stopped".into(),0));
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}

/// #186: the admitted attempt records the measured bytes of exactly the body
/// sent, by source, and `usage get --attempt` reads them back.
#[tokio::test]
async fn admitted_attempt_records_the_exact_sent_body_bytes_by_source() {
    use crate::model::wire_bytes::Category;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (root, owner, control, accepted) =
        fixture(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let path = root.path().join("evidence.txt");
    std::fs::write(&path, b"file fixture").unwrap();
    add_human_attachment(&control, &accepted, path).await;
    let mut snapshot = ready(&control, &accepted).await;
    let mut prepared = control
        .attachments
        .prepare_at_revision(
            &accepted.thread_id,
            CancellationToken::new(),
            false,
            snapshot.selection,
        )
        .await
        .unwrap();
    let instructions: Arc<str> = Arc::from("Working instructions 日本語");
    let (mut request, tags) =
        super::request::prepare_request(&snapshot, Some(&instructions), &mut prepared).unwrap();
    model::configure_shell_description(&mut request, control.store.config.shell.describe_calls);
    let client = HttpModelClient::new(control.store.config.connections["fixture"].clone()).unwrap();
    snapshot.input_bytes = client
        .input_bytes(request.clone(), &tags, Some(&accepted.thread_id))
        .unwrap();
    let measured = snapshot.input_bytes.clone().unwrap();
    for category in [
        Category::System,
        Category::Instructions,
        Category::User,
        Category::Attachment,
        Category::Notice,
        Category::Other,
    ] {
        assert!(measured.get(category) > 0, "{category:?}");
    }
    let attempt = id();
    assert!(
        admit_attempt(&control, &snapshot, &attempt, Arc::new(prepared), 0, 0)
            .await
            .unwrap()
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        let length = loop {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(at) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..at]);
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= at + 4 + length {
                    assert_eq!(bytes.len(), at + 4 + length);
                    break length;
                }
            }
        };
        let response = json!({"id":"fixture","status":"completed","output":[]}).to_string();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
        length
    });
    crate::test_watchdog::watchdog(
        "src/control/driver/preparation_tests.rs: admitted_attempt_records_the_exact_sent_body_bytes_by_source",
        client.response_observed(
            client.prepare_response(request, Some(&accepted.thread_id)),
            CancellationToken::new(),
            None,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let sent = server.await.unwrap();
    assert_eq!(measured.total, sent as u64);
    let saved = control
        .store
        .database
        .call(move |db| Ok(crate::usage_totals::read_attempt(db, &attempt).unwrap()))
        .await
        .unwrap();
    assert_eq!(saved["input_bytes"], measured.to_value());
    assert_eq!(saved["input_bytes"]["total"], sent);
    drop(snapshot);
    close(control, owner).await;
}

#[tokio::test]
async fn cancelled_reply_notice_reaches_next_admission_without_changing_current_path() {
    let (_root, owner, control, first) = fixture("http://127.0.0.1:9/v1").await;
    let accepted: MessageAccepted = serde_json::from_value(
        control
            .store
            .execute(
                Request::new(
                    format!("{}:expected", owner.instance_id),
                    Command::MessageAsk(crate::protocol::MessageAskArgs {
                        to: None,
                        human: false,
                        sender_label: None,

                        deliver: crate::protocol::Deliver::Next,
                        thread_id: Some(first.thread_id.clone()),
                        text: "synthetic reply duty".into(),
                    }),
                ),
                None,
            )
            .await
            .unwrap()
            .result,
    )
    .unwrap();
    let admitted = ready(&control, &accepted).await;
    let result = control
        .store
        .execute(
            Request::new(
                format!("{}:cancel-duty", owner.instance_id),
                Command::MessageCancel(crate::protocol::MessageCancelArgs {
                    message_ids: vec![accepted.message_id.clone()],
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(result["items"][0]["outcome"], "reply_cancelled");
    let thread = accepted.thread_id.clone();
    let event = format!("reply-cancelled-{}", accepted.message_id);
    let before = event.clone();
    let selection = admitted.selection;
    control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                history::selection_revision(db, &thread, history::ThreadScope::Any)?,
                selection
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM delivery WHERE event_id=?1",
                    [before],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    let delivered = ready(&control, &accepted).await;
    assert!(delivered.events.contains(&event));
    // Repeated preparation reuses the same model-visible event node.
    ready(&control, &accepted).await;
    let node = format!("event-{event}");
    control
        .store
        .database
        .call(move |db| {
            let body: String =
                db.query_row("SELECT payload_json FROM node WHERE id=?1", [&node], |r| {
                    r.get(0)
                })?;
            assert!(body.contains("reply_cancelled"));
            assert!(body.contains(&accepted.message_id));
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM thread_path WHERE node_id=?1",
                    [node],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn unknown_arguments_are_ignored_with_a_warning_and_other_calls_continue() {
    let (root, owner, control, accepted) = fixture_with_policy("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let slot = DriverSlot {
        id: id(),
        run_id: accepted.run_id.clone().unwrap(),
        stop: CancellationToken::new(),
        nudge: Arc::new(Notify::new()),
        foreground: Arc::new(ForegroundSlot::default()),
        turn: Arc::new(Mutex::new(None)),

        driver_active: Arc::new(AtomicBool::new(true)),
        proactive_job: Arc::default(),
        proactive_join_done: Arc::new(tokio::sync::Notify::new()),
    };
    let turn = snapshot.turn.clone();
    control.store.database.call(move |db| {
        db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id=?1),'policy-attempt',?1,0,0,'fixture','fixture',0,'completed','{}','now')", [&turn])?;
        db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('policy-group','policy-attempt',?1,'running','now')", [&turn])?;
        for (i, name) in ["bad", "combined", "duplicate", "encoding", "unknown", "valid"].iter().enumerate() {
            db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('policy-group',?1,?2,'shell','{}','pending')", params![name,i as i64])?;
        }
        Ok(())
    }).await.unwrap();
    let marker = root.path().join("ran");
    let command = format!("printf yes > '{}'", marker.display());
    let duplicate_marker = root.path().join("duplicate-ran");
    let duplicate_command = format!("printf bad > '{}'", duplicate_marker.display());
    let duplicate_args = format!(
        r#"{{"command":"true","command":{}}}"#,
        json!(duplicate_command)
    );
    let response = model::ModelResponse::completed(json!({"status":"completed","output":[
        {"type":"function_call","name":"shell","call_id":"bad","arguments":"{\"command\":1}"},
        {"type":"function_call","name":"shell","call_id":"combined","arguments":"{\"command\":1,\"timeout\":2}"},
        {"type":"function_call","name":"shell","call_id":"duplicate","arguments":duplicate_args},
        {"type":"function_call","name":"shell","call_id":"encoding","arguments":json!({"command":"true","timeout":2,"input_encoding":"unsupported-encoding","initial_stdin":"x"}).to_string()},
        {"type":"function_call","name":"shell","call_id":"unknown","arguments":json!({"command":"true","timeout":2}).to_string()},
        {"type":"function_call","name":"shell","call_id":"valid","arguments":json!({"command":command}).to_string()}
    ]}),true).unwrap();
    run_tool_group(&control, &snapshot, &slot, "policy-group", response.calls)
        .await
        .unwrap();
    assert!(
        marker.exists(),
        "valid call did not run in warning-only mode"
    );
    assert!(
        !duplicate_marker.exists(),
        "duplicate call ran in warning-only mode"
    );
    let rows = control.store.database.call(|db| {
        let mut q = db.prepare("SELECT r.call_id, r.status, n.payload_json FROM tool_result r JOIN node n ON n.id=r.node_id WHERE r.group_id='policy-group' ORDER BY r.call_id")?;
        let rows = q.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).await.unwrap();
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0].1, "error");
    assert!(rows[0].2.contains("command"));
    assert_eq!(rows[1].1, "error");
    assert!(rows[1].2.contains("command"));
    assert!(rows[1].2.contains("timeout"));
    assert_eq!(rows[2].1, "error");
    assert!(rows[2].2.contains("Duplicate shell parameter command"));
    assert_eq!(rows[3].1, "error");
    for field in ["timeout", "input_encoding", "initial_stdin"] {
        assert!(rows[3].2.contains(field), "missing {field}: {}", rows[3].2);
    }
    assert!(rows[4].2.contains("timeout"));
    assert_ne!(rows[4].1, "error");
    assert!(rows[4].2.contains("Ignored unknown"));
    assert_ne!(rows[5].1, "error");
    drop(snapshot);
    close(control, owner).await;
}

#[tokio::test]
async fn an_active_run_keeps_its_request_settings_after_reload() {
    let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let pinned = snapshot.clients.clone();
    let original = model_client(&control, &snapshot).unwrap();
    let mut changed = (*control.store.config).clone();
    changed.connections.get_mut("fixture").unwrap().base_url = "http://127.0.0.1:2/v1".into();
    changed
        .connections
        .get_mut("fixture")
        .unwrap()
        .extra_body
        .insert("synthetic_generation".into(), json!("new"));
    control.store.clients.adopt(&changed, None);
    let Next::Ready(refreshed) =
        next_with_clients(&control, &snapshot.thread, &snapshot.run, pinned.clone())
            .await
            .unwrap()
    else {
        panic!("expected preparing Run")
    };
    let body = json!({"model":"synthetic","input":[],"tools":[]});
    assert_eq!(
        original.wire_request(body.clone(), None).unwrap().0,
        model_client(&control, &refreshed)
            .unwrap()
            .wire_request(body.clone(), None)
            .unwrap()
            .0
    );
    assert_eq!(
        control
            .store
            .clients
            .get("fixture")
            .unwrap()
            .wire_request(body, None)
            .unwrap()
            .0["synthetic_generation"],
        "new"
    );
    close(control, owner).await;
    drop(root);
}

#[tokio::test]
async fn extra_instructions_raw_json_and_saved_raw_snapshot_never_become_internal_bundles() {
    for body in [
        r#"{"shellboy_instruction_bundle_v1":true,"local":"must remain literal JSON"}"#,
        r#"{"shellboy_instruction_bundle_v1":true,"extra":[{"path":"fake","body":"not an instruction source"}]}"#,
        "shellboy-saved-instruction-bundle-v1\n{\"extra\":[],\"local\":\"literal prefix\"}",
    ] {
        let (root, owner, control, accepted) = fixture("http://127.0.0.1:1").await;
        let snapshot = ready(&control, &accepted).await;
        std::fs::write(&snapshot.agents, body).unwrap();
        let fresh = turn_instructions(&control, &snapshot)
            .await
            .unwrap()
            .unwrap();
        let items = super::request::leading_items(&snapshot.agents, Some(&fresh)).unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items[1]["content"][0]["text"]
                .as_str()
                .unwrap()
                .ends_with(body)
        );
        // Reopen the saved snapshot through the exact driver read boundary, after file changes.
        std::fs::write(&snapshot.agents, "changed on disk").unwrap();
        let replay = turn_instructions(&control, &snapshot)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh, replay);
        let details = control
            .store
            .database
            .call({
                let target = snapshot.thread.clone();
                move |db| Ok(super::super::db::thread_details(db, &target))
            })
            .await
            .unwrap()
            .unwrap();
        assert!(details.instruction_sources.is_none());
        // Standalone request inspection uses the same persisted kind, not body JSON fields.
        let inspection = crate::request_inspect::execute(
            crate::request_inspect::RequestOptions {
                thread_id: snapshot.thread.clone(),
                attempt_id: None,
                compare_attempt_id: None,
                body: true,
            },
            Some(root.path().join("config.toml")),
            Some(root.path().join("data")),
            root.path(),
        )
        .await;
        assert_eq!(inspection["ok"], true, "{inspection}");
        close(control, owner).await;
    }
}

#[tokio::test]
async fn ordinary_admission_and_completion_preserve_long_model_names_with_gc_enabled_and_disabled()
{
    for auto in [0, 1000] {
        for model in ["m".repeat(257), "m".repeat(8192), "\u{e9}".repeat(257)] {
            let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
            control
                .store
                .execute(
                    Request::new(
                        format!("{}:{}", owner.instance_id, id()),
                        Command::ThreadSet(Box::new(ThreadSetArgs {
                            thread_id: Some(accepted.thread_id.clone()),
                            model: Some(model.clone()),
                            auto_compact_at: Some(auto.to_string()),
                            ..Default::default()
                        })),
                    ),
                    None,
                )
                .await
                .unwrap();
            let snapshot = ready(&control, &accepted).await;
            assert_eq!(snapshot.model, model);
            let mut prepared = control
                .attachments
                .prepare_at_revision(
                    &accepted.thread_id,
                    CancellationToken::new(),
                    false,
                    snapshot.selection,
                )
                .await
                .unwrap();
            let (request, _) =
                super::request::prepare_request(&snapshot, None, &mut prepared).unwrap();
            let prepared = Arc::new(prepared);
            let attempt = id();
            assert!(
                admit_attempt(&control, &snapshot, &attempt, prepared.clone(), 0, 0)
                    .await
                    .unwrap()
            );
            assert_eq!(request["model"], model);
            let response=model::ModelResponse::completed(json!({"id":"long-model-response","status":"completed","usage":{"input_tokens":123,"output_tokens":1},"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"synthetic answer"}]}]}),true).unwrap();
            model::request_path::persist_received_attempt(
                &control.store.database,
                &attempt,
                model::request_path::AttemptKind::Conversation,
                false,
                &Ok(response.clone()),
            )
            .await
            .unwrap();
            assert!(
                adopt_response(
                    &control,
                    &snapshot,
                    &attempt,
                    &response,
                    prepared,
                    None,
                    Vec::new()
                )
                .await
                .unwrap()
            );
            let thread = accepted.thread_id.clone();
            control.store.database.call(move |db| {
                let saved:(String,String)=db.query_row("SELECT model_id,state FROM model_attempt WHERE id=?1",[&attempt],|r|Ok((r.get(0)?,r.get(1)?)))?;
                assert_eq!(saved,(model,"completed".into()));
                assert_eq!(crate::compaction::a_state::read(db,&thread)?.unwrap().reading(),crate::compaction::a_state::Reading::Known(123));
                assert_eq!(db.query_row("SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND n.kind='assistant_response'",[&thread],|r|r.get::<_,i64>(0))?,1);
                assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE state='applied'",[],|r|r.get::<_,i64>(0))?,1);
                assert_eq!(db.query_row("SELECT count(*) FROM automatic_compaction",[],|r|r.get::<_,i64>(0))?,0,"ordinary admission/completion must not create a GC attempt");
                Ok(())
            }).await.unwrap();
            drop(snapshot);
            close(control, owner).await;
        }
    }
}

async fn enter_synthetic_recovery_wait(
    control: &Arc<Control>,
    accepted: &MessageAccepted,
    snapshot: &Snapshot,
) -> String {
    let prepared = Arc::new(
        control
            .attachments
            .prepare_at_revision(
                &accepted.thread_id,
                CancellationToken::new(),
                false,
                snapshot.selection,
            )
            .await
            .unwrap(),
    );
    let attempt = id();
    assert!(
        admit_attempt(control, snapshot, &attempt, prepared, 0, 0)
            .await
            .unwrap()
    );
    let thread = accepted.thread_id.clone();
    let run = accepted.run_id.clone().unwrap();
    let turn = accepted.turn_id.clone().unwrap();
    let saved_attempt = attempt.clone();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            assert_eq!(
                model::request_path::fail_attempt(
                    &tx,
                    &saved_attempt,
                    &json!({"kind":"network","message":"synthetic pre-response failure"}),
                    "synthetic-failure",
                )?,
                super::super::state::AttemptTransition::Applied,
            );
            assert!(super::super::state::recover_turn_tx(
                &tx,
                &thread,
                &run,
                &turn,
                0,
                &ApiError::new(ErrorCode::IoFailed, "synthetic recovery boundary"),
            )?);
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    attempt
}

async fn seed_completed_outgoing_attachment(
    control: &Arc<Control>,
    instance: &str,
    root: &std::path::Path,
    accepted: &MessageAccepted,
    attempt: &str,
) -> (String, String, String, String) {
    let destination = control
        .store
        .execute(
            Request::new(
                format!("{instance}:attachment-destination"),
                Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    cwd: None,
                    model: None,
                    effort: None,
                    parent_thread_id: None,
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let node = id();
    let attachment = id();
    let group = id();
    let call = "settlement-attachment-call".to_owned();
    let execution = id();
    let operation = format!("{instance}:outgoing-attachment-{}", id());
    let origin = json!({
        "kind":"shell","thread_id":accepted.thread_id,"run_id":accepted.run_id,
        "turn_id":accepted.turn_id,"execution_id":execution
    })
    .to_string();
    let run = accepted.run_id.clone().unwrap();
    let turn = accepted.turn_id.clone().unwrap();
    let attempt = attempt.to_owned();
    let target = destination.clone();
    let path = root
        .join("synthetic-outgoing.txt")
        .to_string_lossy()
        .into_owned();
    let source = node.clone();
    let saved_node = node.clone();
    let saved_operation = operation.clone();
    let saved_origin = origin.clone();
    control.store.database.call(move |db| {
        let tx = db.transaction()?;
        crate::history::append_node(
            &tx, &target, &saved_node, None, "event",
            &json!({"kind":"synthetic_outgoing_attachment","content":"OUTGOING_ATTACHMENT_BODY"}),
            "synthetic-outgoing",
        )?;
        tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES(?1,?2,?3,'completed','synthetic-group')",rusqlite::params![group,attempt,turn])?;
        tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES(?1,?2,0,'shell','{}','completed',?3)",rusqlite::params![group,call,execution])?;
        tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?2,?3,'completed')",rusqlite::params![group,call,source])?;
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,output_complete,end_reason)VALUES(?1,?2,?3,?1,'exited','/','{}','UTF-8','UTF-8','closed','synthetic-start','synthetic-end',1,'exited')",rusqlite::params![execution,run,turn])?;
        tx.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES(?1,?2,?3,'file','external','synthetic-attachment')",rusqlite::params![attachment,source,path])?;
        tx.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order,protected)VALUES(?1,?2,'active','synthetic-selection',0,1)",rusqlite::params![target,attachment])?;
        tx.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at,target_run_id)VALUES(?1,?2,'accepted',?3,?4,'synthetic-delivery',NULL)",rusqlite::params![source,target,saved_operation,saved_origin])?;
        assert_eq!(tx.query_row("SELECT status FROM attachment_delivery WHERE operation_id=?1", [&saved_operation], |r|r.get::<_,String>(0))?, "accepted");
        assert!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tool_group_recovery_candidate WHERE group_id=?1)",
                [&group],
                |r| r.get::<_, bool>(0)
            )?
        );
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    (destination, node, operation, origin)
}

#[tokio::test]
async fn recovery_rewind_stops_exact_waiting_run_and_preserves_held_input_body_and_receipt() {
    let (root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let _snapshot = ready(&control, &accepted).await;
    let thread = accepted.thread_id.clone();
    let turn = accepted.turn_id.clone().unwrap();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            crate::history::append_node(
                &tx,
                &thread,
                "recovery-tail",
                Some(&turn),
                "assistant_response",
                &json!({"role":"assistant","content":"tail"}),
                "synthetic-tail",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let accepted_input = control
        .store
        .execute(
            Request::new(
                format!("{}:admitted-input", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: true,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(accepted.thread_id.clone()),
                    text: "ADMITTED_NOT_APPLIED_SENTINEL".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let admitted_id = accepted_input["message_id"].as_str().unwrap().to_owned();
    let snapshot = match next(
        &control,
        &accepted.thread_id,
        accepted.run_id.as_deref().unwrap(),
    )
    .await
    .unwrap()
    {
        Next::Ready(snapshot) => snapshot,
        _ => panic!("human input should keep the synthetic Run ready"),
    };
    let receipt_id = admitted_id.clone();
    let admitted_before = control.store.database.call(move |db| Ok(db.query_row(
        "SELECT state,path_admission,run_id,turn_id,result_json FROM receipt WHERE message_node_id=?1",
        [&receipt_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?))
    )?)).await.unwrap();
    assert_eq!(admitted_before.0, "accepted");
    assert_eq!(admitted_before.1, "admitted");
    let admitted_payload_id = admitted_id.clone();
    let admitted_body_before: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT payload_json FROM node WHERE id=?1",
                [&admitted_payload_id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let failed_attempt = enter_synthetic_recovery_wait(&control, &accepted, &snapshot).await;
    let finish_id = failed_attempt.clone();
    let finish_before = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT state,error_json,ended_at FROM model_attempt WHERE id=?1",
                [&finish_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )?)
        })
        .await
        .unwrap();
    let held = control
        .store
        .execute(
            Request::new(
                format!("{}:held", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(accepted.thread_id.clone()),
                    text: "RECOVERY_INPUT_SENTINEL".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let held_id = held["message_id"].as_str().unwrap().to_owned();
    let detail = control
        .request(Request::new(
            format!("{}:recovery-hold", owner.instance_id),
            Command::MessageGet(crate::protocol::MessageTarget {
                message_id: held_id.clone(),
            }),
        ))
        .await
        .result
        .unwrap();
    assert_eq!(detail["held_by"], json!({"kind":"recovery_wait"}));
    let before_held = {
        let id = held_id.clone();
        control.store.database.call(move |db| Ok((
            db.query_row("SELECT payload_json FROM node WHERE id=?1", [&id], |r| r.get::<_,String>(0))?,
            db.query_row("SELECT state,run_id,turn_id,result_json FROM receipt WHERE message_node_id=?1", [&id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?)))?,
        ))).await.unwrap()
    };
    let (destination, outgoing_node, outgoing_operation, outgoing_origin) =
        seed_completed_outgoing_attachment(
            &control,
            &owner.instance_id,
            root.path(),
            &accepted,
            &failed_attempt,
        )
        .await;
    let outgoing_body_id = outgoing_node.clone();
    let outgoing_body_before = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT payload_json FROM node WHERE id=?1",
                [&outgoing_body_id],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    let response = control
        .request(Request::new(
            format!("{}:atomic-rewind", owner.instance_id),
            Command::ThreadRewind(crate::protocol::ThreadRewindArgs {
                thread_id: Some(accepted.thread_id.clone()),
                node_id: accepted.message_id.clone(),
                exclude_node: false,
                dry_run: false,
                recovery_run_id: accepted.run_id.clone(),
            }),
        ))
        .await;
    assert!(response.ok, "{response:?}");
    let branch = response.result.as_ref().unwrap()["branch_thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let thread = accepted.thread_id.clone();
    let run = accepted.run_id.clone().unwrap();
    let tail = branch.clone();
    let before_body = before_held.0;
    let before_receipt = before_held.1;
    let original_message = accepted.message_id.clone();
    let original_turn = accepted.turn_id.clone().unwrap();
    let finish_after_id = failed_attempt.clone();
    let pending_message = held_id.clone();
    let admitted_after_id = admitted_id.clone();
    let admitted_body_expected = admitted_body_before.clone();
    let admitted_receipt_expected = admitted_before.clone();
    let outgoing_operation_after = outgoing_operation.clone();
    let outgoing_origin_expected = outgoing_origin.clone();
    let outgoing_node_expected = outgoing_node.clone();
    let outgoing_destination = destination.clone();
    let outgoing_body_expected = outgoing_body_before.clone();
    control.store.database.call(move |db| {
        let state: (String, String, i64) = db.query_row("SELECT r.state,u.state,u.generation FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id=?1", [&run], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(state, ("stopped".into(), "stopped".into(), 1));
        let path: Vec<String> = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?.query_map([&thread], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        assert_eq!(path, [original_message]);
        let body: String = db.query_row("SELECT payload_json FROM node WHERE id=?1", [&held_id], |r| r.get(0))?;
        let receipt: (String, Option<String>, Option<String>, Option<String>) = db.query_row("SELECT state,run_id,turn_id,result_json FROM receipt WHERE message_node_id=?1", [&held_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!(receipt.2.as_deref(), Some(original_turn.as_str()));
        assert_eq!((body, receipt), (before_body, before_receipt));
        let admitted_body: String = db.query_row("SELECT payload_json FROM node WHERE id=?1", [&admitted_after_id], |r|r.get(0))?;
        let admitted_receipt: (String,String,Option<String>,Option<String>,Option<String>,Option<String>) = db.query_row(
            "SELECT state,path_admission,hold_reason,run_id,turn_id,result_json FROM receipt WHERE message_node_id=?1",
            [&admitted_after_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))
        )?;
        assert_eq!(admitted_receipt.0, "applied");
        assert_eq!(admitted_receipt.1, "admitted");
        assert_eq!(admitted_receipt.2.as_deref(), Some("terminal_after_path_admission"));
        assert_eq!(admitted_receipt.3.as_deref(), Some(run.as_str()));
        assert_eq!(admitted_receipt.4.as_deref(), Some(original_turn.as_str()));
        assert_eq!(admitted_receipt.5, admitted_receipt_expected.4);
        assert_eq!(admitted_body, admitted_body_expected);
        assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE thread_id=?1 AND node_id=?2", [&tail,&admitted_after_id], |r|r.get::<_,i64>(0))?, 1);
        let delivery: (String,String,String) = db.query_row("SELECT status,thread_id,origin_json FROM attachment_delivery WHERE operation_id=?1", [&outgoing_operation_after], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(delivery, ("held".into(), outgoing_destination, outgoing_origin_expected));
        let outgoing_body_after: String = db.query_row("SELECT payload_json FROM node WHERE id=?1", [&outgoing_node_expected], |r|r.get(0))?;
        assert_eq!(outgoing_body_after, outgoing_body_expected);
        let finish_after = db.query_row("SELECT state,error_json,ended_at FROM model_attempt WHERE id=?1", [&finish_after_id], |r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?)))?;
        assert_eq!(finish_after, finish_before, "original failed attempt FINISH is immutable");
        assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE thread_id=?1 AND node_id='recovery-tail'", [&tail], |r| r.get::<_,i64>(0))?, 1);
        Ok(())
    }).await.unwrap();
    let replacement = control
        .store
        .execute(
            Request::new(
                format!("{}:after-rewind", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: true,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(accepted.thread_id.clone()),
                    text: "AFTER_REWIND_SENTINEL".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let replacement_run = replacement["run_id"].as_str().unwrap().to_owned();
    let replacement_message = replacement["message_id"].as_str().unwrap().to_owned();
    assert_ne!(replacement_run, accepted.run_id.as_deref().unwrap());
    assert!(matches!(
        next(&control, &accepted.thread_id, &replacement_run)
            .await
            .unwrap(),
        Next::Ready(_)
    ));
    let thread = accepted.thread_id.clone();
    let original_message = accepted.message_id.clone();
    control
        .store
        .database
        .call(move |db| {
            let path: Vec<String> = db
                .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
                .query_map([&thread], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let original_at = path
                .iter()
                .position(|node| node == &original_message)
                .unwrap();
            let pending_at = path
                .iter()
                .position(|node| node == &pending_message)
                .unwrap();
            let replacement_at = path
                .iter()
                .position(|node| node == &replacement_message)
                .unwrap();
            assert!(
                original_at < pending_at && pending_at < replacement_at,
                "{path:?}"
            );
            assert!(
                !path.iter().any(|node| node == "recovery-tail"),
                "old tail survived rewind: {path:?}"
            );
            let live_runs: i64 = db.query_row(
                "SELECT count(*) FROM run WHERE thread_id=?1 AND state='live'",
                [&thread],
                |r| r.get(0),
            )?;
            assert_eq!(live_runs, 1, "one replacement Run only");
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_rewind_refuses_retained_driver_without_cancelling_or_mutating() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let _snapshot = ready(&control, &accepted).await;
    let failed_attempt = enter_synthetic_recovery_wait(&control, &accepted, &_snapshot).await;
    let thread = accepted.thread_id.clone();
    let run = accepted.run_id.clone().unwrap();
    let slot = control
        .live
        .register_or_nudge_driver(&thread, run.clone(), &control.stop)
        .await
        .unwrap();
    let response = control
        .request(Request::new(
            format!("{}:refused-rewind", owner.instance_id),
            Command::ThreadRewind(crate::protocol::ThreadRewindArgs {
                thread_id: Some(thread.clone()),
                node_id: accepted.message_id.clone(),
                exclude_node: false,
                dry_run: false,
                recovery_run_id: Some(run.clone()),
            }),
        ))
        .await;
    assert!(!response.ok);
    assert!(
        !slot.stop.is_cancelled(),
        "refusal must not cancel retained owner"
    );
    let attempt = failed_attempt.clone();
    let state: (String, String, i64, String) = control.store.database.call(move |db| Ok(db.query_row(
        "SELECT r.state,u.state,u.generation,(SELECT state FROM model_attempt WHERE id=?2) FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id=?1", rusqlite::params![run,attempt], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))
    )?)).await.unwrap();
    assert_eq!(
        state,
        ("live".into(), "recovery_wait".into(), 0, "failed".into())
    );
    assert!(
        control
            .live
            .remove_driver_if_current(&thread, &slot.id)
            .await
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_rewind_group_boundary_failure_rolls_back_stop_path_branch_and_receipt() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let _snapshot = ready(&control, &accepted).await;
    let turn = accepted.turn_id.clone().unwrap();
    let thread = accepted.thread_id.clone();
    control.store.database.call(move |db| {
        let tx = db.transaction()?;
        crate::history::append_node(&tx, &thread, "synthetic-call", Some(&turn), "assistant_response",
            &json!({"item":{"type":"function_call","call_id":"atomic-call","name":"shell","arguments":"{}"}}), "call")?;
        crate::history::append_node(&tx, &thread, "synthetic-result", Some(&turn), "tool_result",
            &json!({"item":{"type":"function_call_output","call_id":"atomic-call","output":"done"}}), "result")?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let snapshot = ready(&control, &accepted).await;
    let failed_attempt = enter_synthetic_recovery_wait(&control, &accepted, &snapshot).await;
    let held = control
        .store
        .execute(
            Request::new(
                format!("{}:rollback-held", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(accepted.thread_id.clone()),
                    text: "ROLLBACK_BODY_SENTINEL".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let held_id = held["message_id"].as_str().unwrap().to_owned();
    let held_before_id = held_id.clone();
    let held_before = control
        .store
        .database
        .call(move |db| {
            Ok((
                db.query_row(
                    "SELECT payload_json FROM node WHERE id=?1",
                    [&held_before_id],
                    |r| r.get::<_, String>(0),
                )?,
                db.query_row(
                    "SELECT state,run_id,turn_id,result_json FROM receipt WHERE message_node_id=?1",
                    [&held_before_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                        ))
                    },
                )?,
            ))
        })
        .await
        .unwrap();
    let (_destination, _outgoing_node, outgoing_operation, _outgoing_origin) =
        seed_completed_outgoing_attachment(
            &control,
            &owner.instance_id,
            _root.path(),
            &accepted,
            &failed_attempt,
        )
        .await;
    let outgoing_operation_before = outgoing_operation.clone();
    let outgoing_before = control.store.database.call(move |db| Ok(db.query_row(
        "SELECT status,thread_id,origin_json,(SELECT payload_json FROM node WHERE id=attachment_delivery.node_id) FROM attachment_delivery WHERE operation_id=?1",
        [&outgoing_operation_before], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))
    )?)).await.unwrap();
    assert_eq!(outgoing_before.0, "accepted");
    let before_path = accepted.thread_id.clone();
    let before_run = accepted.run_id.clone().unwrap();
    let before = control.store.database.call(move |db| {
        let path: Vec<String> = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?.query_map([&before_path], |r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let state: (String,String,i64) = db.query_row("SELECT r.state,u.state,u.generation FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id=?1", [&before_run], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let branches: i64 = db.query_row("SELECT count(*) FROM thread WHERE parent_thread_id=?1 AND branch_kind='rewind'", [&before_path], |r|r.get(0))?;
        let finish: (String,Option<String>,Option<String>) = db.query_row("SELECT state,error_json,ended_at FROM model_attempt WHERE turn_id=(SELECT id FROM turn WHERE run_id=?1)", [&before_run], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        Ok((path,state,branches,finish))
    }).await.unwrap();
    let response = control
        .request(Request::new(
            format!("{}:rollback-rewind", owner.instance_id),
            Command::ThreadRewind(crate::protocol::ThreadRewindArgs {
                thread_id: Some(accepted.thread_id.clone()),
                node_id: "synthetic-call".into(),
                exclude_node: false,
                dry_run: false,
                recovery_run_id: accepted.run_id.clone(),
            }),
        ))
        .await;
    assert!(!response.ok, "cutting an incomplete tool group must refuse");
    let error = response.error.as_ref().unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(
        error.message,
        "fork would split tool calls from their complete results"
    );
    let after_thread = accepted.thread_id.clone();
    let run = accepted.run_id.clone().unwrap();
    let after = control.store.database.call(move |db| {
        let path: Vec<String> = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?.query_map([&after_thread], |r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let state: (String,String,i64) = db.query_row("SELECT r.state,u.state,u.generation FROM run r JOIN turn u ON u.run_id=r.id WHERE r.id=?1", [&run], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let branches: i64 = db.query_row("SELECT count(*) FROM thread WHERE parent_thread_id=?1 AND branch_kind='rewind'", [&after_thread], |r|r.get(0))?;
        let finish: (String,Option<String>,Option<String>) = db.query_row("SELECT state,error_json,ended_at FROM model_attempt WHERE turn_id=(SELECT id FROM turn WHERE run_id=?1)", [&run], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let attachment: (String,String,String,String) = db.query_row("SELECT status,thread_id,origin_json,(SELECT payload_json FROM node WHERE id=attachment_delivery.node_id) FROM attachment_delivery WHERE operation_id=?1", [&outgoing_operation], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!(attachment,outgoing_before);
        let body: String = db.query_row("SELECT payload_json FROM node WHERE id=?1", [&held_id], |r|r.get(0))?;
        let receipt: (String,Option<String>,Option<String>,Option<String>) = db.query_row("SELECT state,run_id,turn_id,result_json FROM receipt WHERE message_node_id=?1", [&held_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        assert_eq!((body,receipt),held_before);
        Ok((path,state,branches,finish))
    }).await.unwrap();
    assert_eq!(
        after, before,
        "all writes before canonical group validation must roll back"
    );
    drop(snapshot);
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_rewind_terminal_reply_creates_run_after_path_rewrite() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let _initial = ready(&control, &accepted).await;
    let thread = accepted.thread_id.clone();
    let turn = accepted.turn_id.clone().unwrap();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            crate::history::append_node(
                &tx,
                &thread,
                "reply-rewind-tail",
                Some(&turn),
                "assistant_response",
                &json!({"role":"assistant","content":"tail"}),
                "tail",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = ready(&control, &accepted).await;
    let _failed_attempt = enter_synthetic_recovery_wait(&control, &accepted, &snapshot).await;
    let origin = crate::protocol::Origin {
        instance_id: owner.instance_id.clone(),
        thread_id: accepted.thread_id.clone(),
        run_id: accepted.run_id.clone().unwrap(),
        turn_id: accepted.turn_id.clone().unwrap(),
        execution_id: "synthetic-reply-sender".into(),
        token: "synthetic-token".into(),
    };
    let sender = crate::protocol::MessageSender::recorded(Some(&origin), None, false);
    let sender_json = serde_json::to_string(&sender).unwrap();
    let reply_node = id();
    let reply_id = reply_node.clone();
    let source_thread = accepted.thread_id.clone();
    let source_turn = accepted.turn_id.clone().unwrap();
    let old_run = accepted.run_id.clone().unwrap();
    control.store.database.call(move |db| {
        let tx = db.transaction()?;
        crate::history::append_node(&tx, &source_thread, &reply_node, Some(&source_turn), "user",
            &json!({"role":"user","content":[{"type":"input_text","text":"pending model reply duty"}]}), "reply")?;
        tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,path_admission,created_at,result_json)VALUES(?1,?1,?2,?3,?1,?4,?5,'applied','admitted','synthetic-reply',?6)",rusqlite::params![reply_node,sender_json,source_thread,old_run,source_turn,json!({"expect_reply":true,"to_human":false,"reply":{"state":"pending"}}).to_string()])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();

    // Run the saved writer transaction without applying wakes so no driver/provider starts.
    let effects = control
        .store
        .execute(
            Request::new(
                format!("{}:reply-order-rewind", owner.instance_id),
                Command::ThreadRewind(crate::protocol::ThreadRewindArgs {
                    thread_id: Some(accepted.thread_id.clone()),
                    node_id: accepted.message_id.clone(),
                    exclude_node: false,
                    dry_run: false,
                    recovery_run_id: accepted.run_id.clone(),
                }),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        effects.quiet_wake_threads.as_slice(),
        std::slice::from_ref(&accepted.thread_id)
    );
    let thread = accepted.thread_id.clone();
    let old_run = accepted.run_id.clone().unwrap();
    let reply_node_after = reply_id.clone();
    let (new_run, path, receipt, event) = control.store.database.call(move |db| {
        let new_run: String = db.query_row("SELECT id FROM run WHERE thread_id=?1 AND state='live'", [&thread], |r|r.get(0))?;
        let path: Vec<String> = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?.query_map([&thread], |r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let old_state: String = db.query_row("SELECT state FROM run WHERE id=?1", [&old_run], |r|r.get(0))?;
        assert_eq!(old_state, "stopped");
        let receipt: (String,Option<String>,Option<String>,String,String) = db.query_row("SELECT state,run_id,turn_id,sender_json,result_json FROM receipt WHERE message_node_id=?1", [&reply_node_after], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let event: (String,Option<String>,String) = db.query_row("SELECT kind,target_run_id,payload_ref_json FROM event WHERE id=?1", [format!("reply-outcome-{reply_node_after}")], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        Ok((new_run,path,receipt,event))
    }).await.unwrap();
    assert_eq!(
        path.as_slice(),
        std::slice::from_ref(&accepted.message_id),
        "new Run is created only after canonical rewind"
    );
    assert_eq!(receipt.0, "applied");
    assert_eq!(
        receipt.1.as_deref(),
        Some(accepted.run_id.as_deref().unwrap())
    );
    assert_eq!(
        receipt.2.as_deref(),
        Some(accepted.turn_id.as_deref().unwrap())
    );
    assert_eq!(
        serde_json::from_str::<crate::protocol::MessageSender>(&receipt.3).unwrap(),
        sender
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&receipt.4).unwrap()["reply"]["state"],
        "ignored"
    );
    assert_eq!(event.0, "reply_outcome");
    assert_eq!(event.1.as_deref(), Some(new_run.as_str()));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&event.2).unwrap()["message_id"],
        reply_id
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn received_attempt_is_not_adopted_after_interrupt_and_run_carries_next_turn() {
    received_attempt_fence(false).await;
    received_attempt_fence(true).await;
}

async fn received_attempt_fence(truncated: bool) {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let snapshot = ready(&control, &accepted).await;
    let prepared = Arc::new(
        control
            .attachments
            .prepare_at_revision(
                &snapshot.thread,
                CancellationToken::new(),
                false,
                snapshot.selection,
            )
            .await
            .unwrap(),
    );
    let attempt = "received-before-interrupt";
    assert!(
        admit_attempt(&control, &snapshot, attempt, prepared.clone(), 0, 0)
            .await
            .unwrap()
    );
    let response = if truncated {
        let raw =
            json!({"id":"received","status":"incomplete","usage":{"input_tokens":17},"output":[]});
        control
            .store
            .database
            .call(move |db| {
                model::request_path::incomplete_attempt(
                    db,
                    attempt,
                    &json!({"error":{"kind":"output_limit"},"raw_response":raw}),
                    &now(),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        model::ModelResponse::truncated(
            model::http::AttemptFailure::from(model::ModelError::new("output_limit", "fixture")),
            crate::protocol::AttemptOutputLimit {
                stop: true,
                sent_max_tokens: Some(1234),
                source: Some(crate::protocol::OutputLimitSource::ThreadOverride),
                connection: "fixture".into(),
            },
        )
    } else {
        let response = model::ModelResponse::completed(json!({"id":"received","status":"completed","usage":{"input_tokens":17},"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"not selected"}]}]}), false).unwrap();
        model::request_path::persist_received_attempt(
            &control.store.database,
            attempt,
            model::request_path::AttemptKind::Conversation,
            false,
            &Ok(response.clone()),
        )
        .await
        .unwrap();
        response
    };
    let queued = control
        .store
        .execute(
            Request::new(
                format!("{}:queued", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(snapshot.thread.clone()),
                    text: "carry into successor".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap();
    let queued = queued.result["message_id"].as_str().unwrap().to_owned();
    let interruption = super::super::turn_interrupt::admit_locked(
        &control,
        &snapshot.thread,
        &snapshot.turn,
        None,
    )
    .await
    .unwrap();
    assert_eq!(interruption["requested"], true);
    assert!(
        !adopt_response(
            &control,
            &snapshot,
            attempt,
            &response,
            prepared,
            None,
            Vec::new()
        )
        .await
        .unwrap()
    );
    super::super::turn_interrupt::acknowledge(&control, &snapshot.thread, &snapshot.run)
        .await
        .unwrap();
    let (old_turn, run) = (snapshot.turn.clone(), snapshot.run.clone());
    control.store.database.call(move |db| {
        let result = db.query_row("SELECT state,json_extract(input_manifest_json,'$.adoption.outcome'),json_extract(input_manifest_json,'$.adoption.reason'),ended_at FROM model_attempt WHERE id=?1",[attempt],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
        assert_eq!((&*result.0,&*result.1,&*result.2),(if truncated { "incomplete" } else { "completed" },"not_adopted","turn_fenced"));
        assert_eq!(db.query_row("SELECT response_id,json_extract(usage_json,'$.input_tokens') FROM model_attempt WHERE id=?1", [attempt], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?, ("received".into(),17));
        assert!(!result.3.is_empty());
        assert_eq!(db.query_row("SELECT count(*) FROM node WHERE turn_id=?1 AND kind IN ('assistant_response','incomplete_response')",[&old_turn],|r|r.get::<_,i64>(0))?,0);
        assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM receipt q JOIN turn u ON u.id=json_extract(q.result_json,'$.delivery_context.turn_id') JOIN run r ON r.id=u.run_id JOIN thread_path p ON p.thread_id=q.target_thread_id AND p.node_id=q.message_node_id WHERE q.message_node_id=?1 AND q.turn_id=?2 AND q.state='applied' AND q.hold_reason='carried_after_turn_interruption' AND u.id<>?2 AND r.id=?3 AND r.state='live')",params![queued,old_turn,run],|r|r.get::<_,bool>(0))?);
        Ok(())
    }).await.unwrap();
    close(control, owner).await;
}

#[tokio::test]
async fn extra_instruction_warning_survives_snapshot_replay_and_live_config_reconciliation() {
    use crate::protocol::{ConfigChange, InstructionWarningReason, StatusArgs};
    let (root, owner, control, accepted) = fixture("http://127.0.0.1:1").await;
    let path = root.path().join("config.toml");
    let missing = root.path().join("missing.md");
    let good = root.path().join("good.md");
    std::fs::write(&good, "valid common instructions").unwrap();
    let changes = vec![ConfigChange {
        field: "extra_instructions".into(),
        value: Some(json!([missing, good])),
    }];
    let edited = format!(
        "extra_instructions = [ {}, {} ] # hand-edited\n{}",
        json!(missing),
        json!(good),
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, &edited).unwrap();
    assert!(control.store.config.current().extra_instructions.is_empty());
    let hash = crate::digest::sha256_hex(&edited);
    let applied = crate::config_edit::apply_live(
        &path,
        &hash,
        &changes,
        &control.store.config,
        &control.store.clients,
    )
    .unwrap();
    assert_eq!(applied.result["written"], false);
    assert_eq!(applied.result["changes"][0]["field"], "extra_instructions");
    assert_eq!(applied.result["applied_to_running_owner"], true);
    assert_eq!(applied.result["restart_required"], false);
    let again = crate::config_edit::apply_live(
        &path,
        &hash,
        &changes,
        &control.store.config,
        &control.store.clients,
    )
    .unwrap();
    assert_eq!(again.result["applied_to_running_owner"], false);
    assert_eq!(again.result["changes"], json!([]));

    let snapshot = ready(&control, &accepted).await;
    let prompt = turn_instructions(&control, &snapshot)
        .await
        .unwrap()
        .unwrap();
    let items = super::request::leading_items(&snapshot.agents, Some(&prompt)).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[1].to_string().contains("valid common instructions"));
    std::fs::write(&missing, "made available after preparation").unwrap();
    assert_eq!(
        turn_instructions(&control, &snapshot)
            .await
            .unwrap()
            .unwrap(),
        prompt
    );
    let thread_id = snapshot.thread.clone();
    let run_id = snapshot.run.clone();
    let instance = owner.instance_id.clone();
    let (thread, status, point) = control
        .store
        .database
        .call(move |db| {
            use crate::status::observation::{self, Key, Kind, TextBudget};
            let thread = super::super::db::thread_details(db, &thread_id).unwrap();
            let status = crate::status::snapshot(
                db,
                &StatusArgs {
                    run_id: Some(run_id.clone()),
                    all: true,
                    ..StatusArgs::default()
                },
                &instance,
            )
            .unwrap();
            let raw = observation::read_status_tx(
                db,
                &[Key {
                    kind: Kind::Run,
                    id: run_id,
                }],
                &mut TextBudget::default(),
            )?;
            Ok((thread, status, observation::prepare(raw)))
        })
        .await
        .unwrap();
    assert_eq!(thread.instruction_warnings.len(), 1);
    assert_eq!(thread.instruction_warnings[0].path, missing);
    assert_eq!(
        thread.instruction_warnings[0].reason,
        InstructionWarningReason::NotFound
    );
    let run = status
        .page
        .items
        .iter()
        .find(|item| item.kind == "run")
        .unwrap();
    assert_eq!(run.instruction_warnings, thread.instruction_warnings);
    assert!(thread.recovery_reason.is_none());
    assert_eq!(
        point["items"][0]["current"]["instruction_warnings"],
        json!(run.instruction_warnings)
    );
    close(control, owner).await;
}
