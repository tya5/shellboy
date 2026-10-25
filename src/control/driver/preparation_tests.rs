use super::*;
use crate::attachments::PlannedAttachment;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
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
        admit_attempt(&control, &refreshed, &attempt, Arc::new(prepared))
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
    timeout(
        Duration::from_secs(5),
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
        admit_attempt(&control, &snapshot, "invalid-unbound", Arc::new(prepared))
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
    };
    assert_eq!(
        timeout(
            Duration::from_secs(5),
            drive(control.clone(), accepted.thread_id.clone(), slot.clone())
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        ErrorCode::ConfigInvalid
    );
    for _ in 0..3 {
        timeout(
            Duration::from_secs(5),
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
        admit_attempt(&control, &snapshot, "cas-conflict", Arc::new(prepared))
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
            prepared.clone()
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
        admit_attempt(&control, &snapshot, "late-cancelled", prepared.clone())
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
    let body = json!({"error":{"kind":"late"},"partial_text":"saved partial","completed_items":[],"raw_response":{"fixture":"raw bytes represented as JSON"}});
    recover(
        &control,
        &snapshot,
        ApiError::new(ErrorCode::IoFailed, "late failure"),
        Some(("late-cancelled".into(), body.clone())),
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
            admit_attempt(&control, &snapshot, "tool-attempt", prepared.clone())
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
        let args=serde_json::from_value(json!({"command":format!("printf duplicate > '{}'",marker.display()),"background":true})).unwrap();
        let slot = DriverSlot {
            id: id(),
            run_id: accepted.run_id.clone().unwrap(),
            stop: CancellationToken::new(),
            nudge: Arc::new(Notify::new()),
            foreground: Arc::new(ForegroundSlot::default()),
        };
        assert_eq!(
            run_shell(control.clone(), &snapshot, &slot, "group", "call", args)
                .await
                .unwrap_err()
                .code,
            ErrorCode::OperationConflict
        );
        assert!(!marker.exists());
        control
            .store
            .database
            .call(|db| {
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
        admit_attempt(&control, &snapshot, "unadmitted", prepared.clone())
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
