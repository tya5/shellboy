use super::*;
use crate::attachments::PlannedAttachment;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub(super) async fn fixture(
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
                    parent_id: None,
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
                    sender_label: None,
                    now: false,
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
                parent_id: None,
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
    let response = model::ModelResponse::completed(json!({"id":"response","status":"completed","output":[{"type":"function_call","name":"shell","call_id":"call","arguments":"{\"command\":\"printf unused\"}"}]}),true).unwrap();
    assert!(
        adopt_response(
            &control,
            &snapshot,
            "deduplicated-attempt",
            &response,
            prepared.clone(),
            Some("first-group".into())
        )
        .await
        .unwrap()
    );
    assert!(
        !adopt_response(
            &control,
            &snapshot,
            "deduplicated-attempt",
            &response,
            prepared.clone(),
            Some("duplicate-group".into())
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
async fn obsolete_recovery_keeps_raw_audit_without_reviving_cancelled_attempt_or_path() {
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
            super::super::state::cancel_run_attempts(
                &tx,
                &tx.query_row("SELECT id FROM run", [], |r| r.get::<_, String>(0))?,
                "stopped",
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
    let mut body = json!({"error":{"kind":"late"},"partial_text":"saved partial","completed_items":[],"raw_response":{"fixture":"raw bytes represented as JSON"}});
    recover(
        &control,
        &snapshot,
        ApiError::new(ErrorCode::IoFailed, "late failure"),
        Some(("late-cancelled".into(), body.clone())),
    )
    .await
    .unwrap();
    body["attempt_id"] = json!("late-cancelled");
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
            assert_eq!(saved, ("cancelled".into(), "stopped".into(), None));
            assert_eq!(
                db.query_row("SELECT state FROM turn", [], |r| r.get::<_, String>(0))?,
                "stopped"
            );
            let audit = db.query_row(
                "SELECT payload_json FROM node WHERE kind='incomplete_response'",
                [],
                |r| r.get::<_, String>(0),
            )?;
            assert_eq!(serde_json::from_str::<Value>(&audit).unwrap(), body);
            assert_eq!(
                db.query_row(
                    "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                before.0 + 1
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
async fn settled_run_and_turn_completion_never_rewrite_answer_or_ending_time() {
    let (_root, owner, control, accepted) = fixture("http://127.0.0.1:1/v1").await;
    let thread = accepted.thread_id.clone();
    let run = accepted.run_id.clone().unwrap();
    let turn = accepted.turn_id.clone().unwrap();
    let answer = accepted.message_id.clone();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            tx.execute("UPDATE receipt SET state='applied'", [])?;
            for run_state in ["completed", "stopped", "failed", "stopping"] {
                for turn_state in ["completed", "stopped", "failed"] {
                    tx.execute(
                        "UPDATE run SET state=?1,ended_at='original-run'",
                        [run_state],
                    )?;
                    tx.execute(
                        "UPDATE turn SET state=?1,ended_at='original-turn',answer_node_id=?2",
                        params![turn_state, answer],
                    )?;
                    super::super::state::complete_idle_run(&tx, &thread, &run)?;
                    super::super::state::complete_answer_turn(&tx, &thread, &run, &turn, &answer)?;
                    assert_eq!(
                        tx.query_row("SELECT state,ended_at FROM run", [], |r| Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?
                        )))?,
                        (run_state.into(), "original-run".into())
                    );
                    assert_eq!(
                        tx.query_row("SELECT state,ended_at FROM turn", [], |r| Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?
                        )))?,
                        (turn_state.into(), "original-turn".into())
                    );
                }
            }
            tx.execute("UPDATE run SET state='completed'", [])?;
            tx.execute("UPDATE turn SET state='completed'", [])?;
            assert_eq!(
                super::super::state::complete_answer_turn(
                    &tx,
                    &thread,
                    &run,
                    &turn,
                    "different-answer"
                )
                .unwrap_err()
                .into_api_error()
                .code,
                ErrorCode::OperationConflict
            );
            assert_eq!(
                tx.query_row("SELECT answer_node_id FROM turn", [], |r| r
                    .get::<_, String>(0))?,
                answer
            );
            assert_eq!(
                super::super::state::complete_idle_run(&tx, &thread, "missing")
                    .unwrap_err()
                    .into_api_error()
                    .code,
                ErrorCode::TargetUnavailable
            );
            assert_eq!(
                super::super::state::complete_answer_turn(&tx, &thread, &run, "missing", &answer)
                    .unwrap_err()
                    .into_api_error()
                    .code,
                ErrorCode::TargetUnavailable
            );
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
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
        assert!(
            adopt_response(
                &control,
                &snapshot,
                "result-attempt",
                &response,
                prepared,
                Some("result-group".into()),
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
        assert!(
            adopt_response(
                &control,
                &snapshot,
                "tool-attempt",
                &response,
                prepared.clone(),
                Some("group".into())
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
    };
    let binding = slot.bind_turn(&control, &snapshot.turn).await.unwrap();
    let mut gate = binding.start_gate.lock().await;
    let turn = snapshot.turn.clone();
    control.store.database.call(move|db|{
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('gate-attempt',?1,0,0,'fixture','fixture',0,'completed','{}','now')",[&turn])?;
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
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('orphan-attempt',?1,0,0,'fixture','fixture',0,'completed','{}','now')", [&saved_turn])?;
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
