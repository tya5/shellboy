use shellboy::{
    app::DataOwner,
    control::Control,
    frontend,
    ipc::LocalServer,
    ui::{DraftSnapshot, ReceiptStatus, UiAction, UiUpdate, ViewAnchor},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frontend_first_send_is_saved_and_later_draft_survives_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:9/v1\"\n").unwrap();
    let data = root.path().join("data");
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (actions_send, actions_receive) = tokio::sync::mpsc::channel(16);
    let (updates_send, mut updates_receive) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        actions_receive,
        updates_send,
        root.path().into(),
        owner,
        server,
    ));
    actions_send
        .send(UiAction::Send {
            generation: 0,
            now: false,
            snapshot: None,
            draft_id: "first-draft".into(),
            revision: 0,
            thread_id: None,
            cwd: root.path().into(),
            text: "saved message".into(),
            attachments: vec![],
            connection: None,
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    let thread = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match updates_receive.try_recv() {
                Ok(UiUpdate::SavedMessage {
                    status: ReceiptStatus::Accepted,
                    thread_id,
                    ..
                }) => break thread_id,
                Ok(UiUpdate::Receipt {
                    status: ReceiptStatus::Accepted,
                    thread_id: Some(id),
                    ..
                }) => break id,
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    next_update(&mut updates_receive, |update| match update {
        UiUpdate::Thread {
            details,
            draft: None,
            ..
        } if details.thread_id == thread && details.title == "saved message" => Some(()),
        _ => None,
    })
    .await;
    // Stop only after the immutable user node has entered the selected path.
    // Its terminal receipt records admission, not cancellation or an answer.
    next_update(&mut updates_receive, |update| match update {
        UiUpdate::History { nodes, .. }
            if nodes
                .iter()
                .any(|node| node.role == "user" && node.text.contains("saved message")) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    actions_send
        .send(UiAction::Cancel {
            thread_id: thread.clone(),
        })
        .await
        .unwrap();
    next_update(&mut updates_receive, |update| match update {
        UiUpdate::Activity {
            thread_id,
            snapshot,
            ..
        } if thread_id == thread
            && snapshot.phase == shellboy::ui::activity::ActivityPhase::Stopped =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    let draft = DraftSnapshot {
        draft_id: "next-draft".into(),
        revision: 3,
        text: "next unsent text".into(),
        cursor: 16,
        ..DraftSnapshot::default()
    };
    actions_send
        .send(UiAction::SaveDraft {
            thread_id: thread.clone(),
            draft,
            anchor: ViewAnchor::default(),
            follow: false,
        })
        .await
        .unwrap();
    actions_send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let log = std::fs::read_to_string(data.join("logs/shellboy.log")).unwrap();
    assert!(!log.contains("saved message"));
    assert!(!log.contains("next unsent text"));
    let traces = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|line| line["event"] == "message_send" && line["send"]["draft_id"] == "first-draft")
        .collect::<Vec<_>>();
    assert_eq!(
        traces
            .iter()
            .map(|line| line["send"]["stage"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["queued", "processing", "saved"]
    );
    let operation = traces[0]["send"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        traces
            .iter()
            .all(|line| line["send"]["operation_id"] == operation && line["send"]["revision"] == 0)
    );
    let saved_message = traces[2]["send"]["message_id"].as_str().unwrap().to_owned();
    let owner = DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    let values = owner
        .db
        .call(move |db| {
            let correlated: String = db.query_row(
                "SELECT message_node_id FROM receipt WHERE client_operation_id=?1",
                [operation],
                |row| row.get(0),
            )?;
            assert_eq!(correlated, saved_message);
            let (binding, admission): (String,String) = db.query_row(
                "SELECT json_extract(result_json,'$.draft_submission'),path_admission FROM receipt WHERE message_node_id=?1",
                [&saved_message], |row|Ok((row.get(0)?,row.get(1)?)),
            )?;
            let binding:serde_json::Value=serde_json::from_str(&binding).unwrap();
            assert_eq!(admission,"admitted");
            assert_eq!(binding["source_draft_id"],"first-draft");
            assert_eq!(binding["source_revision"],0);
            assert_eq!(binding["message_id"],saved_message);
            assert!(!binding.to_string().contains("saved message"));
            assert_eq!(db.query_row("SELECT count(*) FROM draft WHERE thread_id=?1 AND state='submitted'",[&thread],|r|r.get::<_,i64>(0))?,0);
            let body:String=db.query_row("SELECT payload_json FROM node WHERE id=?1",[saved_message],|r|r.get(0))?;
            assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["content"][0]["text"],"saved message");
            Ok((
                db.query_row("SELECT count(*) FROM thread", [], |r| r.get::<_, i64>(0))?,
                db.query_row(
                    "SELECT body_json FROM draft WHERE thread_id=?1 AND state='editing'",
                    [thread],
                    |r| r.get::<_, String>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(values.0, 1);
    assert_eq!(
        serde_json::from_str::<DraftSnapshot>(&values.1)
            .unwrap()
            .text,
        "next unsent text"
    );
    owner.shutdown().await.unwrap();
}

#[test]
fn copy_code_blocks_follow_markdown_order_indentation_and_eof() {
    let original = "prose\n```rust\n日本語\n\tcode\n```\n~~~text\nsecond\n~~~~\n```\nunclosed";
    assert_eq!(
        frontend::fenced_code_block(original, 0).as_deref(),
        Some("日本語\n\tcode\n")
    );
    assert_eq!(
        frontend::fenced_code_block(original, 1).as_deref(),
        Some("second\n")
    );
    assert_eq!(
        frontend::fenced_code_block(original, 2).as_deref(),
        Some("unclosed")
    );
    let nested = "- item\n\n    ```rust\n    nested\n    ```\n\n> ~~~\n> quoted\n> ~~~\n\n    indented\n\n  ```\n  trimmed\n  ```\n";
    for (index, expected) in ["nested\n", "quoted\n", "indented\n", "trimmed\n"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            frontend::fenced_code_block(nested, index).as_deref(),
            Some(expected),
            "block {index}"
        );
    }
    assert!(frontend::fenced_code_block(nested, 4).is_none());
    assert_eq!(
        frontend::fenced_code_block("```\r\na\r\n```", 0).as_deref(),
        Some("a\n")
    );
}

struct FixtureClipboard;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn work_overview_follows_the_saved_execution_to_get_and_raw_output() {
    use shellboy::{protocol::*, ui::ManagementFollow};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db|{db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','調査','/','/AGENTS.md','p','m','2026-10-25T00:00:00Z'); INSERT INTO run(id,thread_id,state,created_at,ended_at) VALUES('r','t','completed','2026-10-25T00:00:00Z','2026-10-25T00:01:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at) VALUES('u','t','r','completed','2026-10-25T00:00:00Z','2026-10-25T00:01:00Z'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,exit_code,output_complete,started_at,ended_at,saved_sequence) VALUES('saved-execution','r','u','saved-execution','exited','/','{}','UTF-8','UTF-8','closed',0,1,'2026-10-25T00:00:00Z','2026-10-25T00:01:00Z',1);")?;db.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at) VALUES('saved-execution',0,'stdout',0,?1,'2026-10-25T00:01:00Z')",[b"SAVED_OUTPUT_ONLY".as_slice()])?;Ok(())}).await.unwrap();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    let mut ui =
        shellboy::ui::UiState::new(&shellboy::config::Config::default(), root.path().into());
    for (mode, title, key, value) in [
        (
            ManagementFollow::Inspect,
            "コマンドの詳細",
            "/execution_id",
            "saved-execution",
        ),
        (
            ManagementFollow::Read,
            "コマンド出力の読取り",
            "/text",
            "SAVED_OUTPUT_ONLY",
        ),
    ] {
        send.send(UiAction::Manage(Command::Status(StatusArgs {
            state: vec!["exited".into()],
            ..Default::default()
        })))
        .await
        .unwrap();
        let (request_id, report) = next_update(&mut incoming, |update| match update {
            UiUpdate::StructuredManagement {
                work_status,
                request_id,
                title,
                fields,
                omitted,
            } if title == "作業状態" => {
                assert!(work_status);
                assert!(fields[0].value.contains("調査"));
                Some((
                    request_id,
                    UiUpdate::StructuredManagement {
                        work_status,
                        request_id,
                        title,
                        fields,
                        omitted,
                    },
                ))
            }
            _ => None,
        })
        .await;
        ui.apply(report);
        send.send(UiAction::FollowManagement {
            request_id,
            pointer: "/missing-reference".into(),
            mode,
        })
        .await
        .unwrap();
        let failure = next_update(&mut incoming, |update| match update {
            UiUpdate::Status { text, .. } if text.contains("対応する参照先") => Some(text),
            _ => None,
        })
        .await;
        ui.apply(UiUpdate::Status {
            thread_id: None,
            text: failure,
        });
        assert_eq!(ui.management.as_ref().unwrap().title, "作業状態");
        assert!(
            !ui.event(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('r'),
                    crossterm::event::KeyModifiers::NONE
                )
            ))
            .is_empty(),
            "failed follow keeps Status navigation"
        );
        send.send(UiAction::FollowManagement {
            request_id,
            pointer: "/items/0".into(),
            mode,
        })
        .await
        .unwrap();
        let report = next_update(&mut incoming, |update| match update {
            UiUpdate::StructuredManagement {
                work_status,
                request_id,
                title: actual,
                fields,
                omitted,
            } if actual == title => {
                assert!(!work_status);
                assert!(
                    fields
                        .iter()
                        .any(|field| field.key == key && field.value == value),
                    "{actual}: expected {key}={value}; {fields:?}"
                );
                Some(UiUpdate::StructuredManagement {
                    work_status,
                    request_id,
                    title: actual,
                    fields,
                    omitted,
                })
            }
            _ => None,
        })
        .await;
        ui.apply(report);
        assert!(
            ui.event(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('r'),
                    crossterm::event::KeyModifiers::NONE
                )
            ))
            .is_empty(),
            "successful Inspect/Read clears old Status navigation"
        );
    }
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
impl frontend::ClipboardBackend for FixtureClipboard {
    fn capture(
        &self,
        _: u64,
        _: tokio_util::sync::CancellationToken,
    ) -> frontend::ClipboardFuture<'_, shellboy::attachments::clipboard::ClipboardPixels> {
        Box::pin(async {
            Ok(shellboy::attachments::clipboard::ClipboardPixels {
                width: 1,
                height: 1,
                rgba: vec![0, 100, 255, 255],
            })
        })
    }
    fn copy(
        &self,
        text: String,
        _: shellboy::config::CopyBackend,
        limit: usize,
        _: u64,
        _: tokio_util::sync::CancellationToken,
    ) -> frontend::ClipboardFuture<'_, shellboy::attachments::clipboard::CopyOutcome> {
        Box::pin(async move {
            Ok(
                shellboy::attachments::clipboard::CopyOutcome::TerminalRequest(
                    shellboy::attachments::clipboard::osc52(&text, limit)?,
                ),
            )
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clipboard_image_is_leased_then_sent_atomically_and_copy_uses_original_body() {
    use shellboy::{protocol::*, ui::UiAttachment};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:9/v1\"\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    observer.store.database.call(|db| { db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('wait-thread','/','/AGENTS.md','fixture','fixture-model','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('wait-run','wait-thread','live','now');")?; Ok(()) }).await.unwrap();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run_with_clipboard(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
        std::sync::Arc::new(FixtureClipboard),
    ));
    send.send(UiAction::Command(Command::Wait(WaitArgs {
        conditions: vec![WaitCondition::RunEnded {
            run_id: "wait-run".into(),
        }],
        timeout_ms: 60_000,
    })))
    .await
    .unwrap();
    send.send(UiAction::PasteImage {
        position: 0,
        draft_id: "image-draft".into(),
        revision: 0,
        thread_id: None,
    })
    .await
    .unwrap();
    let attachment: UiAttachment = next_update(&mut incoming, |update| match update {
        UiUpdate::ClipboardAttachment { attachment, .. } => Some(attachment),
        _ => None,
    })
    .await;
    assert!(attachment.path.exists());
    send.send(UiAction::ClipboardDecision {
        draft_id: "image-draft".into(),
        attachment_id: attachment.id.clone(),
        accepted: true,
    })
    .await
    .unwrap();
    send.send(UiAction::Send {
        generation: 0,
        now: false,
        snapshot: None,
        draft_id: "image-draft".into(),
        revision: 1,
        thread_id: None,
        cwd: root.path().into(),
        text: "\u{fffc}exact 日本語\n```\ncopy me\n```".into(),
        attachments: vec![attachment],
        connection: None,
        model: None,
        effort: None,
    })
    .await
    .unwrap();
    let thread: String = next_update(&mut incoming, |update| match update {
        UiUpdate::Receipt {
            status: ReceiptStatus::Accepted,
            thread_id: Some(id),
            ..
        } => Some(id),
        _ => None,
    })
    .await;
    // Acceptance makes the message and its attachment durable. The driver adds
    // it to thread_path later, so copy must not depend on that adoption timing.
    let node = observer
        .store
        .database
        .call(move |db| {
            let node: String = db.query_row(
                "SELECT n.id FROM node n JOIN receipt r ON r.message_node_id=n.id WHERE r.target_thread_id=?1 AND n.kind='user'",
                [thread],
                |row| row.get(0),
            )?;
            let refs: i64 = db.query_row(
                "SELECT count(*) FROM attachment_ref WHERE source_node_id=?1",
                [&node],
                |row| row.get(0),
            )?;
            assert_eq!(refs, 1);
            Ok(node)
        })
        .await
        .unwrap();
    assert_eq!(
        frontend::copy_source(&observer, node.clone(), Some(0))
            .await
            .unwrap(),
        "copy me\n"
    );
    send.send(UiAction::Copy {
        node_id: node.clone(),
        code_block: None,
    })
    .await
    .unwrap();
    let bytes: Vec<u8> = next_update(&mut incoming, |update| match update {
        UiUpdate::TerminalRequest { bytes } => Some(bytes),
        _ => None,
    })
    .await;
    assert_eq!(
        bytes,
        shellboy::attachments::clipboard::osc52(
            "exact 日本語\n```\ncopy me\n```",
            MAX_BODY_BYTES as usize
        )
        .unwrap()
    );
    let notice = next_update(&mut incoming, |update| match update {
        UiUpdate::Status { text, .. } if text.contains("コピー要求") => Some(text),
        _ => None,
    })
    .await;
    assert_eq!(
        notice,
        "OSC52: 本文全体のコピー要求を端末へ送りました（完了未確認）"
    );
    send.send(UiAction::Copy {
        node_id: node.clone(),
        code_block: Some(0),
    })
    .await
    .unwrap();
    let code = next_update(&mut incoming, |update| match update {
        UiUpdate::TerminalRequest { bytes } => Some(bytes),
        _ => None,
    })
    .await;
    assert_eq!(
        code,
        shellboy::attachments::clipboard::osc52("copy me\n", MAX_BODY_BYTES as usize).unwrap()
    );
    let notice = next_update(&mut incoming, |update| match update {
        UiUpdate::Status { text, .. } if text.contains("コピー要求") => Some(text),
        _ => None,
    })
    .await;
    assert_eq!(
        notice,
        "OSC52: コードブロック全体のコピー要求を端末へ送りました（完了未確認）"
    );
    observer
        .store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE node SET payload_json=?1 WHERE id=?2",
                rusqlite::params![
                    serde_json::json!({"text":"x".repeat(MAX_BODY_BYTES as usize+1)}).to_string(),
                    node
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let rows = observer
        .store
        .database
        .call(|db| {
            Ok(
                db.query_row("SELECT id FROM node WHERE kind='user' LIMIT 1", [], |r| {
                    r.get::<_, String>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(
        frontend::copy_source(&observer, rows, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InputLimit
    );
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
async fn next_update<T>(
    receive: &mut std::sync::mpsc::Receiver<UiUpdate>,
    mut select: impl FnMut(UiUpdate) -> Option<T>,
) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match receive.try_recv() {
                Ok(UiUpdate::SavedMessage {
                    draft_id,
                    revision,
                    thread_id,
                    status,
                    history,
                    ..
                }) => {
                    // Product applies both together; these observer assertions
                    // inspect each fact without changing UI delivery ordering.
                    if let Some(value) = select(UiUpdate::Receipt {
                        draft_id,
                        revision,
                        thread_id: Some(thread_id),
                        status,
                        message: None,
                    }) {
                        return value;
                    }
                    if let Some(value) = select(*history) {
                        return value;
                    }
                }
                Ok(update) => {
                    if let Some(value) = select(update) {
                        return value;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn terminal_progress_reports_completion_without_inventing_elapsed_time() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','2026-10-01T00:00:00Z'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-10-01T00:00:00Z','2026-10-01T00:00:05.250Z');")?;Ok(())}).await.unwrap();
    let (send, receive) = std::sync::mpsc::sync_channel(4);
    assert!(!frontend::progress(&control, &send, "t").await.unwrap());
    assert!(!frontend::progress(&control, &send, "t").await.unwrap());
    for _ in 0..2 {
        assert!(matches!(
            receive.try_recv().unwrap(),
            UiUpdate::Progress { state, .. } if state == "完了"
        ));
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_grace_offers_wait_then_explicit_force_without_blocking_input() {
    use std::time::Duration;
    let (send, mut receive) = tokio::sync::mpsc::channel(8);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(64);
    let task = tokio::spawn(async move {
        frontend::wait_for_shutdown(
            &mut receive,
            &updates,
            Duration::from_millis(20),
            std::future::pending(),
        )
        .await
    });
    // An early force request does not bypass the grace/explicit choice.
    send.send(UiAction::ForceQuit).await.unwrap();
    next_update(&mut incoming, |u| {
        matches!(
            u,
            UiUpdate::ShutdownProgress {
                force_available: true,
                ..
            }
        )
        .then_some(())
    })
    .await;
    assert!(!task.is_finished());
    send.send(UiAction::WaitForShutdown).await.unwrap();
    next_update(&mut incoming, |u| {
        matches!(
            u,
            UiUpdate::ShutdownProgress {
                force_available: false,
                ..
            }
        )
        .then_some(())
    })
    .await;
    assert!(!task.is_finished());
    next_update(&mut incoming, |u| {
        matches!(
            u,
            UiUpdate::ShutdownProgress {
                force_available: true,
                ..
            }
        )
        .then_some(())
    })
    .await;
    send.send(UiAction::ForceQuit).await.unwrap();
    next_update(&mut incoming, |u| {
        matches!(u, UiUpdate::ForceShutdown).then_some(())
    })
    .await;
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.details.unwrap()["forced_shutdown"], true);
}

#[tokio::test]
async fn completed_shutdown_after_wait_reports_normal_completion() {
    use std::time::Duration;
    let (send, mut receive) = tokio::sync::mpsc::channel(8);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(64);
    let (done, complete) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        frontend::wait_for_shutdown(&mut receive, &updates, Duration::from_millis(10), async {
            complete.await.unwrap();
            Ok(())
        })
        .await
    });
    next_update(&mut incoming, |u| {
        matches!(
            u,
            UiUpdate::ShutdownProgress {
                force_available: true,
                ..
            }
        )
        .then_some(())
    })
    .await;
    send.send(UiAction::WaitForShutdown).await.unwrap();
    done.send(()).unwrap();
    next_update(&mut incoming, |u| {
        matches!(u, UiUpdate::Shutdown).then_some(())
    })
    .await;
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_adapter_loads_full_original_then_forks_before_first_request_and_displays_management_results()
 {
    use shellboy::{history, protocol::*, ui::UiAttachment};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"http://127.0.0.1:9/v1\"\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let cwd = root.path().to_string_lossy().into_owned();
    let text = "日本語 original\n".repeat(6000);
    let expected = text.clone();
    observer.store.database.call(move|db|{let tx=db.transaction()?;tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('source',?1,?1,'fixture','fixture-model','now')",[cwd])?;history::append_node(&tx,"source","original",None,"user",&serde_json::json!({"role":"user","content":[{"type":"input_text","text":text}]}),"now")?;tx.commit()?;Ok(())}).await.unwrap();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run_with_clipboard(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(FixtureClipboard),
    ));
    let file = root.path().join("reload.txt");
    std::fs::write(&file, "alpha").unwrap();
    let original_mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    send.send(UiAction::Manage(Command::AttachmentAdd(
        AttachmentAddArgs {
            thread_id: Some("source".into()),
            path: file.to_string_lossy().into(),
            kind: AttachmentKind::File,
        },
    )))
    .await
    .unwrap();
    next_update(&mut incoming, |u| {
        matches!(u,UiUpdate::StructuredManagement{ref title,..}if title.contains("添付の追加"))
            .then_some(())
    })
    .await;
    send.send(UiAction::ReloadAttachments {
        thread_id: "source".into(),
    })
    .await
    .unwrap();
    let before = next_update(&mut incoming, |u| match u {
        UiUpdate::StructuredManagement { title, fields, .. } if title.contains("明示再読込み") => {
            Some(fields)
        }
        _ => None,
    })
    .await;
    assert_eq!(field(&before, "/sent"), "false");
    let before_hash = field(&before, "/attachments/0/content_hash");
    assert!(!before_hash.is_empty());
    std::fs::write(&file, "bravo").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    send.send(UiAction::ReloadAttachments {
        thread_id: "source".into(),
    })
    .await
    .unwrap();
    let after = next_update(&mut incoming, |u| match u {
        UiUpdate::StructuredManagement { title, fields, .. } if title.contains("明示再読込み") => {
            Some(fields)
        }
        _ => None,
    })
    .await;
    assert_ne!(field(&after, "/attachments/0/content_hash"), before_hash);
    assert_eq!(field(&after, "/sent"), "false");
    observer
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM model_attempt", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM run", [], |r| r.get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM attachment_send", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    send.send(UiAction::LoadEditRequest {
        thread_id: "source".into(),
        node_id: "original".into(),
        generation: 7,
        draft_revision: 3,
    })
    .await
    .unwrap();
    let loaded = next_update(&mut incoming, |u| match u {
        UiUpdate::EditRequestLoaded {
            thread_id,
            node_id,
            generation,
            draft_revision,
            text,
            attachments,
            run_id,
        } => Some((
            thread_id,
            node_id,
            generation,
            draft_revision,
            text,
            attachments,
            run_id,
        )),
        _ => None,
    })
    .await;
    assert_eq!(loaded.0, "source");
    assert_eq!(loaded.1, "original");
    assert_eq!((loaded.2, loaded.3), (7, 3));
    assert_eq!(loaded.4, expected);
    assert!(loaded.5.is_empty() && loaded.6.is_none());
    let scope = IndexArgs {
        dimensions: None,
        scope: Some(IndexScope::Threads),
        space_id: None,
        thread_ids: vec!["source".into()],
        source_ids: vec![],
    };
    send.send(UiAction::IndexPreview(scope.clone()))
        .await
        .unwrap();
    let preview = next_update(&mut incoming, |u| match u {
        UiUpdate::IndexPreviewReady { args, text } => Some((args, text)),
        _ => None,
    })
    .await;
    assert_eq!(preview.0, scope);
    assert!(preview.1.contains("資料数: 2"));
    let characters = preview
        .1
        .lines()
        .find_map(|line| line.strip_prefix("対象文字数: "))
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(characters > 64 * 1024);
    // A new Run can appear after loading the original. Consent is bound to its ID.
    observer.store.database.call(|db|{db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('late-run','source','live','now')",[])?;Ok(())}).await.unwrap();
    send.send(UiAction::SendEdited {
        generation: 0,
        now: false,
        snapshot: None,
        draft_id: "edited".into(),
        revision: 4,
        source_thread_id: "source".into(),
        source_node_id: "original".into(),
        cwd: root.path().into(),
        text: "edited request".into(),
        attachments: vec![],
        connection: None,
        model: None,
        effort: None,
        confirmed_run_id: None,
    })
    .await
    .unwrap();
    next_update(&mut incoming, |u| match u {
        UiUpdate::EditConfirmationRequired {
            draft_id,
            revision,
            run_id,
        } => {
            assert_eq!(draft_id, "edited");
            assert_eq!(revision, 4);
            assert_eq!(run_id, "late-run");
            Some(())
        }
        _ => None,
    })
    .await;
    next_update(&mut incoming, |u| {
        matches!(
            u,
            UiUpdate::Receipt {
                status: ReceiptStatus::Failed,
                ..
            }
        )
        .then_some(())
    })
    .await;
    observer
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM thread", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT state FROM run WHERE id='late-run'", [], |r| r
                    .get::<_, String>(0))?,
                "live"
            );
            Ok(())
        })
        .await
        .unwrap();
    send.send(UiAction::SendEdited {
        generation: 0,
        now: false,
        snapshot: None,
        draft_id: "edited".into(),
        revision: 4,
        source_thread_id: "source".into(),
        source_node_id: "original".into(),
        cwd: root.path().into(),
        text: "edited request".into(),
        attachments: Vec::<UiAttachment>::new(),
        connection: None,
        model: None,
        effort: None,
        confirmed_run_id: Some("late-run".into()),
    })
    .await
    .unwrap();
    let fork = next_update(&mut incoming, |u| match u {
        UiUpdate::Receipt {
            draft_id,
            thread_id: Some(id),
            status: ReceiptStatus::Accepted,
            ..
        } if draft_id == "edited" => Some(id),
        UiUpdate::Receipt {
            status: ReceiptStatus::Failed,
            message,
            ..
        } => panic!("{message:?}"),
        _ => None,
    })
    .await;
    assert_ne!(fork, "source");
    let forkid = fork.clone();
    let original = observer
        .store
        .database
        .call(move |db| {
            let body = db.query_row(
                "SELECT payload_json FROM node WHERE id='original'",
                [],
                |r| r.get::<_, String>(0),
            )?;
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM thread_path WHERE thread_id=?1 AND node_id='original'",
                    [forkid],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(body)
        })
        .await
        .unwrap();
    assert!(original.contains("original"));
    send.send(UiAction::Manage(Command::StorageStatus(StorageArgs {
        category: None,
    })))
    .await
    .unwrap();
    let shown = next_update(&mut incoming, |u| match u {
        UiUpdate::StructuredManagement { title, fields, .. } if title.contains("保存容量") => {
            Some(fields)
        }
        _ => None,
    })
    .await;
    assert!(!field(&shown, "/logical_live_bytes").is_empty());
    send.send(UiAction::Manage(Command::IndexStatus(IndexArgs {
        dimensions: None,
        scope: Some(IndexScope::All),
        space_id: None,
        thread_ids: vec![],
        source_ids: vec![],
    })))
    .await
    .unwrap();
    let managed = next_update(&mut incoming, |u| match u {
        UiUpdate::StructuredManagement { title, fields, .. } if title.contains("索引の状態") => {
            Some(fields)
        }
        _ => None,
    })
    .await;
    assert!(!field(&managed, "/management/spaces").is_empty());
    assert!(
        managed
            .iter()
            .all(|field| field.key != "/request_operation_id"),
        "index management retains its supplemental projection without a receipt link"
    );
    assert!(
        field(&managed, "/management/coverage/not_indexed_sources")
            .parse::<u64>()
            .is_ok()
    );
    assert!(!field(&managed, "/management/definitions/not_indexed_sources").is_empty());
    send.send(UiAction::Command(Command::ThreadGet(ThreadTarget {
        thread_id: Some(fork),
    })))
    .await
    .unwrap();
    next_update(&mut incoming, |u| match u {
        UiUpdate::StructuredManagement { title, fields, .. } if title.contains("会話の詳細") =>
        {
            assert_eq!(field(&fields, "/model"), "fixture-model");
            Some(())
        }
        _ => None,
    })
    .await;
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn progress_distinguishes_answered_background_and_counts_only_actual_spawns() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    owner.db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','2026-10-01T00:00:00Z','2026-10-01T00:00:02Z'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('spawned','r','u','spawned','running','/','{\"spawned\":true}','UTF-8','UTF-8','closed','now'),('cancelled','r','u','cancelled','stopped','/','{}','UTF-8','UTF-8','closed','now'),('rejected','r','u','rejected','failed','/','{\"spawned\":false}','UTF-8','UTF-8','closed','now');")?;Ok(())}).await.unwrap();
    let (send, receive) = std::sync::mpsc::sync_channel(8);
    assert!(frontend::progress(&control, &send, "t").await.unwrap());
    assert!(
        matches!(receive.try_recv().unwrap(),UiUpdate::Progress{tool_calls:1,state,..}if state=="回答済み背景処理中")
    );
    owner
        .db
        .call(|db| {
            db.execute("UPDATE run SET state='stopping' WHERE id='r'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(frontend::progress(&control, &send, "t").await.unwrap());
    assert!(
        matches!(receive.try_recv().unwrap(),UiUpdate::Progress{state,..}if state=="停止処理中")
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[test]
fn reasoning_ranges_follow_original_unicode_text_and_never_include_opaque_items() {
    let payload = serde_json::json!({"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"考え中\n日本語"}],"encrypted_content":"opaque-secret"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"回答 日本語"}]},{"type":"function_call","call_id":"c","arguments":"{\"command\":\"printf\"}"},{"type":"vendor_extension","text":"unknown-secret"}]});
    let (text, ranges) = frontend::display_payload(&payload);
    assert_eq!(text, shellboy::history::text_of_payload(&payload));
    assert_eq!(ranges.len(), 1);
    let chars = text.chars().collect::<Vec<_>>();
    let (start, end) = ranges[0];
    assert_eq!(
        chars[start..end].iter().collect::<String>(),
        "考え中\n日本語"
    );
    assert!(!text.contains("opaque-secret") && !text.contains("unknown-secret"));
    assert!(text.contains("回答 日本語") && text.contains("printf"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_database_query_does_not_delay_quit_grace_notification() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "[io]\nshutdown_grace_ms=20\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let db = owner.db.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocked = tokio::spawn(async move {
        db.call(move |_| {
            let _ = entered_tx.send(());
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
        .await
    });
    entered_rx.await.unwrap();
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    send.send(UiAction::LoadThreads {
        cursor: None,
        filter: String::new(),
        generation: 1,
    })
    .await
    .unwrap();
    send.send(UiAction::Quit).await.unwrap();
    next_update(&mut incoming, |u| {
        matches!(
            u,
            UiUpdate::ShutdownProgress {
                force_available: true,
                ..
            }
        )
        .then_some(())
    })
    .await;
    assert!(
        !blocked.is_finished(),
        "grace notification must precede releasing the blocked DB worker"
    );
    release_tx.send(()).unwrap();
    blocked.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(10), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn draft_database_save_failure_stops_admission_and_owned_shell_but_retains_input() {
    use shellboy::{protocol::*, ui::UiState};
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let proxy = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..split]);
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if bytes.len() >= split + 4 + length {
                    break;
                }
            }
        }
        let body=serde_json::json!({"id":"draft-save-failure-response","status":"completed","output":[{"type":"function_call","id":"draft-save-failure-item","call_id":"draft-save-failure-call","name":"shell","arguments":serde_json::json!({"command":"printf '%s' \"$$\"; exec sleep 60","background":false}).to_string()}]}).to_string();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='{url}'\n[io]\nterm_grace_ms=50\ndrain_timeout_ms=100\n")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let accepted = control
        .send_draft(
            format!("{}:start", control.store.instance_id),
            ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            },
            None,
            "start shell".into(),
            vec![],
        )
        .await
        .unwrap();
    let thread = accepted["thread_id"].as_str().unwrap().to_owned();
    let (execution,pid)=tokio::time::timeout(Duration::from_secs(5),async {loop {
        let value=owner.db.call(|db|{use rusqlite::OptionalExtension;Ok(db.query_row("SELECT s.id,o.bytes FROM shell_execution s JOIN output_chunk o ON o.execution_id=s.id WHERE s.state='running' AND o.stream='stdout' LIMIT 1",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Vec<u8>>(1)?))).optional()?)}).await.unwrap();
        if let Some((id,bytes))=value {break(id,String::from_utf8(bytes).unwrap().parse::<i32>().unwrap());}
        tokio::time::sleep(Duration::from_millis(5)).await;
    }}).await.unwrap();
    owner.db.call(|db|{db.execute_batch("CREATE TRIGGER fixture_draft_save_failure BEFORE INSERT ON draft BEGIN SELECT RAISE(ABORT,'fixture draft source save failure'); END;")?;Ok(())}).await.unwrap();
    let mut ui = UiState::new(&control.store.config, root.path().into());
    ui.select_thread(thread.clone());
    ui.event(crossterm::event::Event::Paste("未保存😀 input".into()));
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let observer = control.clone();
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    send.send(UiAction::SaveDraft {
        thread_id: thread,
        draft: ui.view().draft.clone(),
        anchor: ViewAnchor::default(),
        follow: true,
    })
    .await
    .unwrap();
    let failure = next_update(&mut incoming, |u| {
        matches!(u, UiUpdate::StorageFailed { .. }).then_some(u)
    })
    .await;
    ui.apply(failure);
    assert_eq!(ui.view().draft.text, "未保存😀 input");
    assert!(ui.view().status.contains("新規実行を停止"));
    assert!(!ui.view().save_retry);
    let response = observer
        .request(Request::new(
            format!("{}:after-failure", observer.store.instance_id),
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        ))
        .await;
    assert_eq!(response.error.unwrap().code, ErrorCode::InvalidState);
    tokio::time::timeout(Duration::from_secs(5),async {loop {
        let id=execution.clone();let done=observer.store.database.call(move|db|Ok(db.query_row("SELECT state NOT IN ('starting','running','stopping') AND output_complete FROM shell_execution WHERE id=?1",[id],|r|r.get::<_,bool>(0))?)).await.unwrap();
        if done {break;}tokio::time::sleep(Duration::from_millis(5)).await;
    }}).await.unwrap();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "owned shell process was reaped"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    send.send(UiAction::Quit).await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(8), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SaveFailed);
    proxy.await.unwrap();
}

#[tokio::test]
async fn direct_database_read_failure_also_stops_new_execution() {
    use shellboy::protocol::*;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    owner.db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('source','/','/AGENTS.md','fixture','fixture','now'); DROP TABLE view_state;")?;Ok(())}).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(8);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    send.send(UiAction::SelectThread {
        preview_sources: vec![],
        thread_id: "source".into(),
        generation: 1,
        view_revision: 0,
        known_view: None,
    })
    .await
    .unwrap();
    next_update(&mut incoming, |u| {
        matches!(u,UiUpdate::LoadFailed{message,..}if message.contains("view_state")).then_some(())
    })
    .await;
    let response = observer
        .request(Request::new(
            format!("{}:after-read-failure", observer.store.instance_id),
            Command::ThreadList(ThreadListArgs::default()),
        ))
        .await;
    assert_eq!(response.error.unwrap().code, ErrorCode::InvalidState);
    send.send(UiAction::Quit).await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SaveFailed);
}

fn field(fields: &[shellboy::presentation::PresentationField], pointer: &str) -> String {
    fields
        .iter()
        .find(|field| field.key == pointer)
        .unwrap_or_else(|| panic!("Missing {pointer}: {fields:?}"))
        .value
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structured_history_pages_details_url_copy_and_wait_cancel_keep_originals() {
    use serde_json::json;
    use shellboy::{
        history,
        presentation::{BlockKind, PresentationKind},
        protocol::*,
    };
    use std::{sync::Arc, time::Duration};
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let mixed = json!({"output":[
        {"type":"reasoning","summary":[{"type":"summary_text","text":"見える推論😀"}],"encrypted_content":"opaque-not-for-display"},
        {"type":"message","content":[{"type":"output_text","text":"## 回答\n[完全URL](https://example.invalid/path?key=value#anchor)"}]},
        {"type":"function_call","name":"shell","call_id":"call-1","arguments":"{\"command\":\"printf plain\"}"}
    ]});
    let long = json!({"output":[{"type":"message","content":[{"type":"output_text","text":format!("# 長い原文\n{}\n末尾", "日本語😀".repeat(18000))}]}]});
    let canonical = history::text_of_payload(&long);
    let tool = json!({"item":{"type":"function_call_output","call_id":"stream-call","output":json!({"execution_id":"recorded-shell","status":"exited","exit_code":0,"output":"saved combined preview"}).to_string()},"group_id":"saved-group"});
    let tool_expected = tool.to_string();
    let tool_for_db = tool.clone();
    let long_for_db = long.clone();
    let mixed_for_db = mixed.clone();
    owner.db.call(move|db|{let tx=db.transaction()?;
        tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('source','/','/AGENTS.md','fixture','fixture','now')",[])?;
        history::append_node(&tx,"source","mixed",None,"assistant_response",&mixed_for_db,"now")?;
        history::append_node(&tx,"source","long",None,"assistant_response",&long_for_db,"now")?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('observed-run','source','live','now')",[])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('recorded-turn','source','observed-run','completed','now','now')",[])?;
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('recorded-shell','observed-run','recorded-turn','recorded-shell-op','exited','/','{}','UTF-8','UTF-8','closed',2,0,1,'now','now')",[])?;
        tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('recorded-shell',0,'stdout',0,?1,'now')",[b"# raw output\n**plain**\n\x1b]8;;https://example.invalid/raw?long=one&keep=two#full\x1b\\raw link\x1b]8;;\x1b\\\n".as_slice()])?;
        let stderr=format!("warning on stderr\n{}\nTRUE_STDERR_TAIL","!".repeat(40_000));
        tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('recorded-shell',1,'stderr',0,?1,'now')",[stderr.as_bytes()])?;
        history::append_node(&tx,"source","stream-node",None,"tool_result",&tool_for_db,"now")?;
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('saved-attempt','recorded-turn',0,0,'fixture','fixture',0,'completed','{}','now')",[])?;
        tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('saved-group','saved-attempt','recorded-turn','completed','now')",[])?;
        tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('saved-group','stream-call',0,'shell','{}','completed','recorded-shell')",[])?;
        tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('saved-group','stream-call','stream-node','exited')",[])?;
        history::append_node(&tx,"source","unknown-stream",None,"tool_result",&json!({"item":{"type":"function_call_output","call_id":"unknown-call","output":json!({"execution_id":"missing-execution","output":"unknown original"}).to_string()}}),"now")?;
        history::append_node(&tx,"source","explicit-streams",None,"tool_result",&json!({"item":{"type":"function_call_output","call_id":"explicit-call","output":json!({"stdout":"known stdout","stderr":"known stderr"}).to_string()}}),"now")?;
        history::append_node(&tx,"source","mixed-streams",None,"tool_result",&json!({"item":{"type":"function_call_output","call_id":"mixed-call","output":json!({"execution_id":"missing-execution","output":"combined original","stderr":"explicit stderr"}).to_string()}}),"now")?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(256);
    let bridge = tokio::spawn(frontend::run_with_clipboard(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
        Arc::new(FixtureClipboard),
    ));
    send.send(UiAction::RenderWidth { width: 60 })
        .await
        .unwrap();
    send.send(UiAction::SelectThread {
        preview_sources: vec![],
        thread_id: "source".into(),
        generation: 3,
        view_revision: 0,
        known_view: None,
    })
    .await
    .unwrap();
    let (nodes, selection_revision) = next_update(&mut incoming, |update| match update {
        UiUpdate::History {
            nodes,
            selection_revision,
            ..
        } => Some((nodes, selection_revision)),
        _ => None,
    })
    .await;
    let stream_node = nodes
        .iter()
        .find(|node| node.node_id == "stream-node")
        .unwrap();
    assert_eq!(stream_node.text, history::text_of_payload(&tool));
    assert!(
        !stream_node
            .presentation
            .blocks
            .iter()
            .any(
                |block| block.stream == Some(shellboy::presentation::OutputStream::Stdout)
                    && block.text.contains("# raw output")
            ),
        "successful output stays closed on history load"
    );
    assert!(
        !stream_node
            .presentation
            .fields
            .iter()
            .any(|field| field.key.starts_with("/ui_raw_stream_preview"))
    );
    send.send(UiAction::LoadToolPreview {
        thread_id: "source".into(),
        generation: 3,
        key: shellboy::ui::ItemKey::Tool {
            group_id: "saved-group".into(),
            call_id: "stream-call".into(),
        },
        request_revision: 17,
        source_node_id: "stream-node".into(),
        group_id: Some("saved-group".into()),
        call_id: "stream-call".into(),
    })
    .await
    .unwrap();
    let preview = next_update(&mut incoming, |update| match update {
        UiUpdate::ToolPreview {
            thread_id,
            generation,
            key,
            request_revision: 17,
            result,
        } => {
            assert_eq!(thread_id, "source");
            assert_eq!(generation, 3);
            assert_eq!(
                key,
                shellboy::ui::ItemKey::Tool {
                    group_id: "saved-group".into(),
                    call_id: "stream-call".into()
                }
            );
            Some(result.unwrap())
        }
        _ => None,
    })
    .await;
    assert!(preview.blocks.iter().any(|block| block.stream
        == Some(shellboy::presentation::OutputStream::Stdout)
        && block.text == "# raw output\n**plain**\nraw link\n"));
    let stderr = preview
        .blocks
        .iter()
        .find(|block| block.stream == Some(shellboy::presentation::OutputStream::Stderr))
        .unwrap();
    assert!(
        stderr.text.ends_with("TRUE_STDERR_TAIL") && !stderr.text.contains("warning on stderr")
    );
    assert!(
        preview.blocks.iter().all(|block| !block.source_exact
            && block.rendered.is_none()
            && block.markdown.is_empty())
    );
    assert!(
        preview
            .blocks
            .iter()
            .map(|block| block.text.len())
            .sum::<usize>()
            <= 16 * 1024
    );
    assert!(preview.notice.as_deref().unwrap().contains("省略"));
    // The exact original/copy route is independent of the abbreviated display.
    let full_url = "https://example.invalid/raw?long=one&keep=two#full".to_owned();
    send.send(UiAction::CopyText {
        text: full_url.clone(),
        scope: shellboy::ui::CopyScope::Value,
    })
    .await
    .unwrap();
    let copied_raw = next_update(&mut incoming, |update| match update {
        UiUpdate::TerminalRequest { bytes } => Some(bytes),
        _ => None,
    })
    .await;
    assert_eq!(
        copied_raw,
        shellboy::attachments::clipboard::osc52(&full_url, 65536).unwrap()
    );
    let explicit = nodes
        .iter()
        .find(|node| node.node_id == "explicit-streams")
        .unwrap();
    assert!(explicit.presentation.blocks.iter().any(|block| block.stream
        == Some(shellboy::presentation::OutputStream::Stdout)
        && block.text == "known stdout"));
    assert!(explicit.presentation.blocks.iter().any(|block| block.stream
        == Some(shellboy::presentation::OutputStream::Stderr)
        && block.text == "known stderr"));
    assert!(
        !explicit
            .presentation
            .fields
            .iter()
            .any(|field| field.key == "/ui_raw_stream_preview"),
        "explicit streams do not require an execution lookup"
    );
    let mixed_fallback = nodes
        .iter()
        .find(|node| node.node_id == "mixed-streams")
        .unwrap();
    assert!(
        mixed_fallback
            .presentation
            .blocks
            .iter()
            .any(
                |block| block.stream == Some(shellboy::presentation::OutputStream::Stderr)
                    && block.text == "explicit stderr"
            )
    );
    assert!(
        mixed_fallback
            .presentation
            .blocks
            .iter()
            .any(
                |block| block.stream == Some(shellboy::presentation::OutputStream::Combined)
                    && block.text == "combined original"
            )
    );
    let unknown = nodes
        .iter()
        .find(|node| node.node_id == "unknown-stream")
        .unwrap();
    assert!(unknown.presentation.blocks.iter().any(|block| block.stream
        == Some(shellboy::presentation::OutputStream::Combined)
        && block.text.contains("unknown original")));
    assert!(
        !unknown
            .presentation
            .fields
            .iter()
            .any(|field| field.key.starts_with("/ui_raw_stream_preview")),
        "unverified execution references do not trigger eager raw reads"
    );
    let mixed_node = nodes.iter().find(|node| node.node_id == "mixed").unwrap();
    assert_eq!(mixed_node.text, history::text_of_payload(&mixed));
    assert_eq!(mixed_node.presentation.kind, PresentationKind::Assistant);
    assert!(
        mixed_node
            .presentation
            .blocks
            .iter()
            .filter_map(|block| block.rendered.as_ref())
            .any(|rendered| rendered.width == 60)
    );
    for kind in [BlockKind::Reasoning, BlockKind::Prose, BlockKind::ToolCall] {
        assert!(
            mixed_node
                .presentation
                .blocks
                .iter()
                .any(|block| block.kind == kind),
            "{kind:?}"
        );
    }
    assert!(!format!("{:?}", mixed_node.presentation).contains("opaque-not-for-display"));
    assert_eq!(
        mixed_node.presentation.links[0].url,
        "https://example.invalid/path?key=value#anchor"
    );
    let tail = nodes.iter().find(|node| node.node_id == "long").unwrap();
    assert!(tail.base_char_offset > 0);
    assert_eq!(
        tail.text,
        canonical
            .chars()
            .skip(tail.base_char_offset)
            .take(tail.text.chars().count())
            .collect::<String>()
    );
    send.send(UiAction::RenderWidth { width: 32 })
        .await
        .unwrap();
    send.send(UiAction::LoadNodeWindow {
        thread_id: "source".into(),
        generation: 3,
        node_id: "long".into(),
        char_offset: 2,
        view_revision: 1,
    })
    .await
    .unwrap();
    let start = next_update(&mut incoming, |update| match update {
        UiUpdate::NodeWindow { node, .. } => Some(node),
        _ => None,
    })
    .await;
    assert_eq!(start.base_char_offset, 0);
    assert!(start.text.starts_with("# 長い原文"));
    assert!(
        start
            .presentation
            .blocks
            .iter()
            .filter_map(|block| block.rendered.as_ref())
            .all(|rendered| rendered.width == 32)
    );
    assert!(
        start
            .presentation
            .blocks
            .iter()
            .all(|block| block.source_start >= start.base_char_offset
                && block.source_end <= start.base_char_offset + start.text.chars().count())
    );
    send.send(UiAction::LoadHistoryWindow {
        thread_id: "source".into(),
        generation: 3,
        view_revision: 2,
        ordinal: 1,
        char_offset: 7000,
        from_end: false,
        selection_revision,
    })
    .await
    .unwrap();
    let (window, anchor) = next_update(&mut incoming, |update| match update {
        UiUpdate::HistoryWindow { nodes, anchor, .. } => Some((nodes, anchor)),
        _ => None,
    })
    .await;
    assert_eq!(anchor.char_offset, 7000);
    assert_eq!(anchor.node_id.as_deref(), Some("long"));
    assert!(window.iter().any(
        |node| node.node_id == "mixed" && node.presentation.kind == PresentationKind::Assistant
    ));
    send.send(UiAction::LoadOriginal {
        node_id: "long".into(),
        cursor: None,
        request_id: 40,
    })
    .await
    .unwrap();
    let first = next_update(&mut incoming, |update| match update {
        UiUpdate::OriginalPage {
            request_id: 40,
            body,
            ..
        } => Some(body),
        _ => None,
    })
    .await;
    assert!(first.text.len() <= 65536);
    assert!(first.next_cursor.is_some());
    assert!(canonical.starts_with(&first.text));
    send.send(UiAction::LoadOriginal {
        node_id: "long".into(),
        cursor: first.next_cursor.clone(),
        request_id: 41,
    })
    .await
    .unwrap();
    let second = next_update(&mut incoming, |update| match update {
        UiUpdate::OriginalPage {
            request_id: 41,
            body,
            ..
        } => Some(body),
        _ => None,
    })
    .await;
    assert!(canonical.starts_with(&(first.text.clone() + &second.text)));
    send.send(UiAction::Manage(Command::HistoryGet(HistoryGetArgs {
        final_answer_only: false,
        node_id: "long".into(),
        cursor: None,
        limit_bytes: MAX_BODY_BYTES,
        encoding: "UTF-8".into(),
    })))
    .await
    .unwrap();
    let (report_id, fields, omitted) = next_update(&mut incoming, |update| match update {
        UiUpdate::StructuredManagement {
            request_id,
            fields,
            omitted,
            ..
        } => Some((request_id, fields, omitted)),
        _ => None,
    })
    .await;
    assert!(omitted);
    assert!(
        fields
            .iter()
            .all(|field| field.key != "/request_operation_id"),
        "receipt-free history reads must not advertise a saved operation result"
    );
    assert!(field(&fields, "/text").contains("詳細"));
    send.send(UiAction::LoadManagementDetail {
        request_id: report_id,
        pointer: "/text".into(),
        cursor: None,
    })
    .await
    .unwrap();
    let detail = next_update(&mut incoming, |update| match update {
        UiUpdate::ManagementDetail {
            request_id,
            text,
            next_cursor,
            ..
        } if request_id == report_id => Some((text, next_cursor)),
        _ => None,
    })
    .await;
    assert!(detail.0.len() <= 65536);
    assert!(detail.0.starts_with(&first.text));
    send.send(UiAction::Manage(Command::HistorySearch(
        HistorySearchArgs {
            query: "offline empty index".into(),
            limit: 10,
            ..Default::default()
        },
    )))
    .await
    .unwrap();
    let empty = next_update(&mut incoming, |update| match update {
        UiUpdate::StructuredManagement { title, fields, .. } if title == "履歴の検索" => {
            Some(fields)
        }
        _ => None,
    })
    .await;
    assert_eq!(field(&empty, "/items"), "0");
    assert!(
        empty
            .iter()
            .all(|field| field.key != "/request_operation_id")
    );
    send.send(UiAction::Manage(Command::ShellRead(ShellReadArgs {
        execution_id: "recorded-shell".into(),
        cursor: None,
        encoding: None,
        limit_bytes: 65536,
    })))
    .await
    .unwrap();
    let raw = next_update(&mut incoming, |update| match update {
        UiUpdate::StructuredManagement { title, fields, .. } if title == "コマンド出力の読取り" => {
            Some(fields)
        }
        _ => None,
    })
    .await;
    assert!(field(&raw, "/text").starts_with("# raw output\n**plain**"));
    assert_eq!(field(&raw, "/output_complete"), "true");
    assert!(!field(&raw, "/cursor").is_empty());
    let url =
        "https://example.invalid/very/long/path?first=日本語&second=value#full-fragment".to_owned();
    send.send(UiAction::CopyText {
        text: url.clone(),
        scope: shellboy::ui::CopyScope::Value,
    })
    .await
    .unwrap();
    let copied = next_update(&mut incoming, |update| match update {
        UiUpdate::TerminalRequest { bytes } => Some(bytes),
        _ => None,
    })
    .await;
    assert_eq!(
        copied,
        shellboy::attachments::clipboard::osc52(&url, MAX_BODY_BYTES as usize).unwrap()
    );
    observer.store.database.call(|db|{db.execute("INSERT INTO app_meta(key,value)VALUES('raw_tombstone.recorded-shell','{\"reason\":\"fixture_expired\"}')",[])?;db.execute("DELETE FROM output_chunk WHERE execution_id='recorded-shell'",[])?;db.execute("UPDATE shell_execution SET retention_generation=1 WHERE id='recorded-shell'",[])?;Ok(())}).await.unwrap();
    send.send(UiAction::LoadNodeWindow {
        thread_id: "source".into(),
        generation: 3,
        node_id: "stream-node".into(),
        char_offset: 0,
        view_revision: 4,
    })
    .await
    .unwrap();
    let fallback = next_update(&mut incoming, |update| match update {
        UiUpdate::NodeWindow { node, .. } if node.node_id == "stream-node" => Some(node),
        _ => None,
    })
    .await;
    assert_eq!(fallback.text, history::text_of_payload(&tool));
    assert!(fallback.presentation.blocks.iter().any(|block| block.stream
        == Some(shellboy::presentation::OutputStream::Combined)
        && block.text.contains("saved combined preview")));
    assert!(
        !fallback
            .presentation
            .fields
            .iter()
            .any(|field| field.key.starts_with("/ui_raw_stream_preview")),
        "successful source hydration still does not eagerly read raw after retention"
    );
    send.send(UiAction::LoadToolPreview {
        thread_id: "source".into(),
        generation: 3,
        key: shellboy::ui::ItemKey::Tool {
            group_id: "saved-group".into(),
            call_id: "stream-call".into(),
        },
        request_revision: 18,
        source_node_id: "stream-node".into(),
        group_id: Some("saved-group".into()),
        call_id: "stream-call".into(),
    })
    .await
    .unwrap();
    let expired = next_update(&mut incoming, |update| match update {
        UiUpdate::ToolPreview {
            request_revision: 18,
            result,
            ..
        } => Some(result),
        _ => None,
    })
    .await;
    assert!(expired.unwrap_err().contains("no longer fully retained"));
    observer
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT payload_json FROM node WHERE id='stream-node'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                tool_expected
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM runtime_pin WHERE resource_kind='shell_output'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    send.send(UiAction::Manage(Command::Wait(WaitArgs {
        conditions: vec![WaitCondition::RunEnded {
            run_id: "observed-run".into(),
        }],
        timeout_ms: 60000,
    })))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = observer
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap();
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    send.send(UiAction::CancelWait).await.unwrap();
    let outcome = next_update(&mut incoming, |update| match update {
        UiUpdate::StructuredManagement { title, fields, .. } if title == "条件の成立を待機" => {
            Some(fields)
        }
        _ => None,
    })
    .await;
    assert_eq!(field(&outcome, "/status"), "interrupted");
    send.send(UiAction::LoadManagementDetail {
        request_id: report_id,
        pointer: "/text".into(),
        cursor: None,
    })
    .await
    .unwrap();
    next_update(&mut incoming, |update| match update {
        UiUpdate::Status { text, .. } if text.contains("管理結果が更新") => Some(()),
        _ => None,
    })
    .await;
    observer
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row("SELECT state FROM run WHERE id='observed-run'", [], |r| {
                    r.get::<_, String>(0)
                })?,
                "live"
            );
            Ok(())
        })
        .await
        .unwrap();
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

// Exercise the real asynchronous bridge without sending a message or starting
// an API request: stale navigation must recover by node identity, not ordinal.
async fn stale_history_navigation_recovers_after_path_change(remove_anchor: bool) {
    use serde_json::json;
    use shellboy::history;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    owner.db.call(|db| {
        let tx = db.transaction()?;
        tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('navigation','/','/AGENTS.md','fixture','fixture-model','now')", [])?;
        for ordinal in 0..80 {
            history::append_node(&tx, "navigation", &format!("nav-{ordinal}"), None, "user", &json!({"text":format!("Saved message {ordinal}: 日本語の本文")}), "now")?;
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    let anchor = ViewAnchor {
        node_id: Some("nav-40".into()),
        char_offset: 5,
    };
    send.send(UiAction::SelectThread {
        preview_sources: vec![],
        thread_id: "navigation".into(),
        generation: 19,
        view_revision: 7,
        known_view: Some((anchor.clone(), false)),
    })
    .await
    .unwrap();
    let old_revision = next_update(&mut incoming, |update| match update {
        UiUpdate::History {
            thread_id,
            selection_revision,
            nodes,
            ..
        } if thread_id == "navigation" => {
            assert!(nodes.iter().any(|node| node.node_id == "nav-40"));
            Some(selection_revision)
        }
        _ => None,
    })
    .await;
    let new_revision = observer.store.database.call(move |db| {
        let tx = db.transaction()?;
        if remove_anchor {
            // A rewind selects a shorter existing prefix; the original nodes
            // remain in the immutable DAG and must not be shown as selected.
            tx.execute("DELETE FROM thread_path WHERE thread_id='navigation' AND ordinal>20", [])?;
            tx.execute("UPDATE thread SET cursor_node_id='nav-20',selection_revision=selection_revision+1 WHERE id='navigation'", [])?;
        } else {
            history::append_node(&tx, "navigation", "nav-80", None, "user", &json!({"text":"Appended without model execution"}), "now")?;
        }
        let revision = tx.query_row("SELECT selection_revision FROM thread WHERE id='navigation'", [], |row| row.get::<_,i64>(0))?;
        tx.commit()?;
        Ok(revision as u64)
    }).await.unwrap();
    assert!(new_revision > old_revision);
    send.send(UiAction::LoadHistoryWindow {
        thread_id: "navigation".into(),
        generation: 19,
        view_revision: 7,
        ordinal: 50,
        char_offset: 0,
        from_end: false,
        selection_revision: old_revision,
    })
    .await
    .unwrap();
    next_update(&mut incoming, |update| match update {
        UiUpdate::HistoryInvalidated {
            thread_id,
            generation,
            view_revision,
        } => {
            assert_eq!(thread_id, "navigation");
            assert_eq!(generation, 19);
            assert_eq!(view_revision, Some(7));
            Some(())
        }
        UiUpdate::LoadFailed { message, .. } => {
            panic!("Stale history must request a rebase, not remain failed: {message}")
        }
        _ => None,
    })
    .await;
    // The UI advances its navigation revision upon invalidation; this is the
    // resulting action crossing the actual UI/frontend boundary.
    send.send(UiAction::RebaseHistory {
        thread_id: "navigation".into(),
        generation: 19,
        view_revision: 8,
        anchor,
    })
    .await
    .unwrap();
    next_update(&mut incoming, |update| match update {
        UiUpdate::HistoryWindow {
            pending: _,
            read_revision: _,
            thread_id,
            generation,
            view_revision,
            nodes,
            first_ordinal,
            selection_revision,
            total_nodes,
            anchor,
        } => {
            assert_eq!(
                (thread_id.as_str(), generation, view_revision),
                ("navigation", 19, 8)
            );
            assert_eq!(selection_revision, new_revision);
            assert!(!nodes.is_empty());
            assert!(nodes.iter().all(|node| !node.text.is_empty()));
            if remove_anchor {
                assert_eq!(first_ordinal, 0);
                assert_eq!(total_nodes, 21);
                assert_eq!(anchor.node_id.as_deref(), Some("nav-0"));
                assert_eq!(anchor.char_offset, 0);
                assert!(!nodes.iter().any(|node| node.node_id == "nav-40"));
                assert_eq!(nodes.last().unwrap().node_id, "nav-20");
            } else {
                assert_eq!(first_ordinal, 30);
                assert_eq!(total_nodes, 81);
                assert_eq!(anchor.node_id.as_deref(), Some("nav-40"));
                assert_eq!(anchor.char_offset, 5);
                assert!(
                    nodes
                        .iter()
                        .any(|node| node.node_id == "nav-40"
                            && node.text.contains("Saved message 40"))
                );
            }
            Some(())
        }
        UiUpdate::LoadFailed { message, .. } => panic!("Rebase failed: {message}"),
        _ => None,
    })
    .await;
    let side_effects = observer
        .store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT COUNT(*) FROM receipt", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                db.query_row("SELECT COUNT(*) FROM model_attempt", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                db.query_row("SELECT COUNT(*) FROM shell_execution", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(
        side_effects,
        (0, 0, 0),
        "Navigation must recover without sending a message or starting model/shell work"
    );
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_history_window_rebases_preserved_anchor_without_sending() {
    stale_history_navigation_recovers_after_path_change(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_history_window_falls_back_when_anchor_leaves_selected_path() {
    stale_history_navigation_recovers_after_path_change(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_output_identity_is_delivered_before_each_history_window() {
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, format!("default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='http://{}/v1'\n", listener.local_addr().unwrap())).unwrap();
    let (complete, ready) = tokio::sync::oneshot::channel();
    let (more, next_delta) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let split = loop {
            let mut bytes = [0; 4096];
            let count = socket.read(&mut bytes).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&bytes[..count]);
            if let Some(at) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let length: usize = String::from_utf8_lossy(&request[..split])
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap();
        while request.len() < split + length {
            let mut bytes = [0; 4096];
            let count = socket.read(&mut bytes).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&bytes[..count]);
        }
        let delta = format!(
            "data: {}\n\n",
            json!({"type":"response.output_text.delta","delta":"ordinary prose"})
        );
        let continuation = [
            json!({"type":"unknown.delta","delta":"ignored"}),
            json!({"type":"response.reasoning_text.delta","delta":"考え"}),
            json!({"type":"response.output_text.delta","delta":" 後半"}),
        ]
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
        // Final text intentionally differs from the delta: reconciliation must
        // use output_item identity, never content equality.
        let final_event = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":{"id":"fixture-response","status":"completed","output":[{"id":"message-1","type":"message","role":"assistant","opaque":"z".repeat(128*1024),"content":[{"type":"output_text","text":"persisted final prose"}]}]}})
        );
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",delta.len()+continuation.len()+final_event.len()).as_bytes()).await.unwrap();
        socket.write_all(delta.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), next_delta)
            .await
            .unwrap()
            .unwrap();
        socket.write_all(continuation.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), ready)
            .await
            .unwrap()
            .unwrap();
        socket.write_all(final_event.as_bytes()).await.unwrap();
    });
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let mut observations = observer.subscribe();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    send.send(UiAction::Send {
        generation: 0,
        now: false,
        snapshot: None,
        draft_id: "identity".into(),
        revision: 0,
        thread_id: None,
        cwd: root.path().into(),
        text: "identity test".into(),
        attachments: vec![],
        connection: None,
        model: None,
        effort: None,
    })
    .await
    .unwrap();
    let (thread, generation, attempt) = next_update(&mut incoming, |update| match update {
        UiUpdate::Stream {
            thread_id,
            generation,
            node_id,
            text,
            ack,
            ..
        } if text.contains("ordinary prose") => {
            if let Some(ack) = ack {
                ack.notify_one();
            }
            Some((thread_id, generation, node_id))
        }
        _ => None,
    })
    .await;
    use shellboy::protocol::{Command, Request, ThreadSetArgs};
    let response = observer
        .request(Request::new(
            format!("{}:metadata-refresh", observer.store.instance_id),
            Command::ThreadSet(ThreadSetArgs {
                agents: None,
                thread_id: Some(thread.clone()),
                title: Some("metadata during stream".into()),
                connection: None,
                model: None,
                effort: None,
                cwd: None,
            }),
        ))
        .await;
    assert!(response.ok);
    // Wait until the metadata-triggered refresh actually reached the UI queue.
    next_update(&mut incoming, |update| match update {
        UiUpdate::History { thread_id, .. } if thread_id == thread => Some(()),
        UiUpdate::Stream { ack: Some(ack), .. } => {
            ack.notify_one();
            None
        }
        _ => None,
    })
    .await;
    more.send(()).unwrap();
    next_update(&mut incoming, |update| match update {
        UiUpdate::Stream {
            text,
            base_char_offset,
            total_chars,
            reasoning_ranges,
            ack,
            ..
        } => {
            if let Some(ack) = ack {
                ack.notify_one();
            }
            if text.ends_with(" 後半") {
                assert_eq!(text, "ordinary prose考え 後半");
                assert_eq!((base_char_offset, total_chars), (0, 19));
                assert_eq!(reasoning_ranges, [(14, 16)]);
                Some(())
            } else {
                None
            }
        }
        _ => None,
    })
    .await;
    let mut offsets = Vec::new();
    while let Ok(event) = observations.try_recv() {
        if let shellboy::control::EngineEvent::Stream {
            stream_char_offset,
            text,
            ..
        } = event
        {
            offsets.push((stream_char_offset, text));
        }
    }
    assert_eq!(
        offsets,
        [
            (0, "ordinary prose".into()),
            (14, "考え".into()),
            (16, " 後半".into())
        ]
    );
    complete.send(()).unwrap();
    let mut observed = None;
    let (node, revision, total) = next_update(&mut incoming, |update| match update {
        UiUpdate::StreamCommitted {
            thread_id,
            generation: actual,
            attempt_id,
            node_id,
        } => {
            assert_eq!(
                (&thread_id, actual, &attempt_id),
                (&thread, generation, &attempt)
            );
            observed = Some(node_id);
            None
        }
        UiUpdate::History {
            nodes,
            selection_revision,
            total_nodes,
            ..
        } if nodes
            .iter()
            .any(|node| node.text.contains("persisted final prose")) =>
        {
            let node = nodes
                .iter()
                .find(|node| node.text.contains("persisted final prose"))
                .unwrap();
            assert_eq!(
                observed.as_deref(),
                Some(node.node_id.as_str()),
                "mapping must precede the completed History"
            );
            Some((node.node_id.clone(), selection_revision, total_nodes))
        }
        _ => None,
    })
    .await;
    let saved_attempt = attempt.clone();
    let saved_node = node.clone();
    observer.store.database.call(move|db|{
        let actual:String=db.query_row("SELECT node_id FROM output_item WHERE attempt_id=?1",[&saved_attempt],|r|r.get(0))?;
        assert_eq!(actual,saved_node);
        // Multiple output items from the same response still yield one mapping.
        db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id) VALUES(?1,1,'reasoning','completed','{}',?2)",rusqlite::params![saved_attempt,saved_node])?;
        Ok(())
    }).await.unwrap();
    // Actual DB -> preparation worker -> frontend verifies both the exact
    // canonical case and a different final body without copying preview bytes
    // into canonical source identity.
    for (text, expected_offset) in [("persisted final prose", 9), ("ordinary prose考え 後半", 0)]
    {
        send.send(UiAction::RebaseStream(Box::new(
            shellboy::ui::StreamRebase {
                thread_id: thread.clone(),
                generation,
                view_revision: 42,
                node_id: node.clone(),
                source: shellboy::ui::UiNode {
                    node_id: attempt.clone(),
                    role: "assistant".into(),
                    text: text.into(),
                    total_chars: text.chars().count(),
                    ..Default::default()
                },
                anchor: ViewAnchor {
                    node_id: Some(attempt.clone()),
                    char_offset: 9,
                },
            },
        )))
        .await
        .unwrap();
        next_update(&mut incoming, |update| match update {
            UiUpdate::HistoryWindow {
                view_revision: 42,
                anchor,
                ..
            } => {
                assert_eq!(anchor.node_id.as_deref(), Some(node.as_str()));
                assert_eq!(anchor.char_offset, expected_offset);
                Some(())
            }
            _ => None,
        })
        .await;
        next_update(&mut incoming, |update| match update {
            UiUpdate::HistoryRebaseNotice {
                message,
                anchor,
                view_revision: 42,
                ..
            } => {
                assert_eq!(anchor.char_offset, expected_offset);
                assert_eq!(message.contains("先頭"), expected_offset == 0);
                Some(())
            }
            _ => None,
        })
        .await;
    }
    for rebase in [false, true] {
        // Drain unrelated activity/refresh updates before this explicit read.
        while incoming.try_recv().is_ok() {}
        let action = if rebase {
            UiAction::RebaseHistory {
                thread_id: thread.clone(),
                generation,
                view_revision: 9,
                anchor: ViewAnchor {
                    node_id: Some(node.clone()),
                    char_offset: 3,
                },
            }
        } else {
            UiAction::LoadHistoryWindow {
                thread_id: thread.clone(),
                generation,
                view_revision: 8,
                ordinal: total - 1,
                char_offset: 3,
                from_end: false,
                selection_revision: revision,
            }
        };
        send.send(action).await.unwrap();
        let mut mappings = Vec::new();
        next_update(&mut incoming, |update| match update {
            UiUpdate::StreamCommitted {
                attempt_id,
                node_id,
                ..
            } => {
                mappings.push((attempt_id, node_id));
                None
            }
            UiUpdate::HistoryWindow {
                view_revision,
                anchor,
                nodes,
                ..
            } if view_revision == if rebase { 9 } else { 8 } => {
                // Completion may also schedule a tail refresh concurrently;
                // repeated authoritative mappings across pages are harmless.
                assert!(!mappings.is_empty(), "mapping must precede each window");
                assert!(
                    mappings
                        .iter()
                        .all(|mapping| mapping == &(attempt.clone(), node.clone()))
                );
                assert_eq!(anchor.node_id.as_deref(), Some(node.as_str()));
                assert!(nodes.iter().any(|item| item.node_id == node));
                Some(())
            }
            _ => None,
        })
        .await;
    }
    // An inconsistent ledger must not arbitrarily identify one response node.
    let ambiguous_attempt = attempt.clone();
    let selected_thread = thread.clone();
    observer.store.database.call(move|db|{
        let tx=db.transaction()?;
        shellboy::history::append_node(&tx,&selected_thread,"ambiguous-node",None,"assistant_response",&json!({"output":[]}),"now")?;
        tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES(?1,2,'message','completed','{}','ambiguous-node')",[ambiguous_attempt])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    while incoming.try_recv().is_ok() {}
    send.send(UiAction::SelectThread {
        preview_sources: vec![],
        thread_id: thread.clone(),
        generation: 17,
        view_revision: 0,
        known_view: None,
    })
    .await
    .unwrap();
    next_update(&mut incoming, |update| match update {
        UiUpdate::StreamCommitted { generation: 17, .. } => {
            panic!("ambiguous output ledger must not choose an identity")
        }
        UiUpdate::History { generation: 17, .. } => Some(()),
        _ => None,
    })
    .await;
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    proxy.await.unwrap();
}

/// Both submission paths share chip decoding while retaining their established error order.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_and_edited_send_keep_cwd_and_attachment_validation_order() {
    use shellboy::ui::UiAttachment;
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let owner = DataOwner::start(None, Some(root.path().join("data")), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(4);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(4);
    let (updates, mut incoming) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    let bad_cwd = || std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 255]));
    let attachment = |position| UiAttachment {
        id: "bad".into(),
        path: "/not-read".into(),
        kind: "unknown-kind".into(),
        position,
        state: "ready".into(),
    };
    for (edited, cwd_bad, bad_position, expected) in [
        (false, true, true, "Thread cwd must be valid Unicode"),
        (true, true, true, "Draft attachment positions"),
        (false, false, true, "Draft attachment positions"),
        (true, false, true, "Draft attachment positions"),
        (false, false, false, "Unknown draft attachment kind"),
        (true, true, false, "Unknown draft attachment kind"),
    ] {
        let cwd = if cwd_bad {
            bad_cwd()
        } else {
            root.path().into()
        };
        let attachments = vec![attachment(Some(if bad_position { 1 } else { 4 }))];
        let action = if edited {
            UiAction::SendEdited {
                generation: 0,
                now: false,
                snapshot: None,
                draft_id: "edited".into(),
                revision: 0,
                source_thread_id: "absent".into(),
                source_node_id: "absent".into(),
                confirmed_run_id: None,
                text: "text\u{fffc}".into(),
                attachments,
                cwd,
                connection: None,
                model: None,
                effort: None,
            }
        } else {
            UiAction::Send {
                generation: 0,
                now: false,
                snapshot: None,
                draft_id: "send".into(),
                revision: 0,
                thread_id: None,
                text: "text\u{fffc}".into(),
                attachments,
                cwd,
                connection: None,
                model: None,
                effort: None,
            }
        };
        send.send(action).await.unwrap();
        let message = next_update(&mut incoming, |update| match update {
            UiUpdate::Receipt {
                status: ReceiptStatus::Failed,
                message,
                ..
            } => message,
            _ => None,
        })
        .await;
        assert!(
            message.starts_with(expected),
            "{edited}/{cwd_bad}/{bad_position}: {message}"
        );
    }
    observer
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM node", [], |row| row.get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM receipt", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM model_attempt", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM draft WHERE state IN ('submitting','submitted')",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    send.send(UiAction::Quit).await.unwrap();
    bridge.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_accepted_before_frontend_binding_still_drains_queued_send_and_latest_draft() {
    use shellboy::protocol::*;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let thread = control
        .store
        .execute(
            Request::new(
                format!("{}:thread", control.store.instance_id),
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
    let (rpc, _) = tokio::sync::mpsc::channel(1);
    let server = LocalServer::bind(&owner, rpc).await.unwrap();
    let (send, receive) = shellboy::ui::actions::channel(2);
    send.try_send(UiAction::Send {
        generation: 0,
        now: false,
        snapshot: None,
        draft_id: "queued-before-quit".into(),
        revision: 0,
        thread_id: Some(thread.clone()),
        cwd: root.path().into(),
        text: "queued instruction survives".into(),
        attachments: vec![],
        connection: None,
        model: None,
        effort: None,
    })
    .unwrap();
    send.try_send(UiAction::SaveDraft {
        thread_id: thread.clone(),
        draft: DraftSnapshot {
            draft_id: "draft".into(),
            revision: 1,
            text: "older draft".into(),
            ..Default::default()
        },
        anchor: ViewAnchor::default(),
        follow: false,
    })
    .unwrap();
    assert!(
        send.try_send_batch(vec![
            UiAction::SaveDraft {
                thread_id: thread.clone(),
                draft: DraftSnapshot {
                    draft_id: "draft".into(),
                    revision: 2,
                    text: "latest unsent draft".into(),
                    ..Default::default()
                },
                anchor: ViewAnchor::default(),
                follow: false
            },
            UiAction::Quit,
        ])
        .is_empty()
    );
    // No frontend has bound the owner yet. Its first poll must close tool
    // starts while preserving the already accepted Send and draft ordering.
    let (updates, _received) = std::sync::mpsc::sync_channel(128);
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reopened = DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    reopened.db.call(move |db| {
        let draft:String=db.query_row("SELECT body_json FROM draft WHERE thread_id=?1 AND state='editing'",[&thread],|r|r.get(0))?;
        use rusqlite::OptionalExtension;
        let (saved,admission,state):(String,String,String)=db.query_row("SELECT result_json,path_admission,state FROM receipt WHERE target_thread_id=?1",[&thread],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let saved:serde_json::Value=serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["draft_submission"]["source_draft_id"],"queued-before-quit");
        assert_eq!(saved["draft_submission"]["source_revision"],0);
        assert!(!saved["draft_submission"].to_string().contains("queued instruction survives"));
        let frozen:Option<String>=db.query_row("SELECT body_json FROM draft WHERE thread_id=?1 AND state='submitted'",[&thread],|r|r.get(0)).optional()?;
        if admission=="admitted" || (admission!="unknown" && state=="cancelled") {
            assert!(frozen.is_none(),"finished copy must be retired");
        } else {
            let frozen:serde_json::Value=serde_json::from_str(&frozen.expect("unadmitted/unknown content must be retained")).unwrap();
            assert_eq!(frozen["text"],"queued instruction survives");
            assert_eq!(frozen["_submission"]["source_draft_id"],"queued-before-quit");
        }
        assert_eq!(serde_json::from_str::<DraftSnapshot>(&draft).unwrap().text,"latest unsent draft");
        assert_eq!(db.query_row("SELECT COUNT(*) FROM receipt WHERE target_thread_id=?1",[&thread],|r|r.get::<_,i64>(0))?,1);
        let (kind,body):(String,String)=db.query_row("SELECT n.kind,n.payload_json FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1",[thread],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(kind,"user");
        let payload:serde_json::Value=serde_json::from_str(&body).unwrap();
        assert_eq!(payload["role"],"user");
        assert_eq!(payload["content"][0]["type"],"input_text");
        assert_eq!(payload["content"][0]["text"],"queued instruction survives");
        assert_eq!(db.query_row("SELECT COUNT(*) FROM shell_execution",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_submission_snapshot_never_creates_receipt_attempt_or_submitted_watermark() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let observer = control.clone();
    let (rpc_send, rpc_receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc_send).await.unwrap();
    tokio::spawn(control.clone().serve(rpc_receive));
    let (send, receive) = tokio::sync::mpsc::channel(8);
    let (out, incoming) = std::sync::mpsc::sync_channel(128);
    let mut incoming = incoming;
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        out,
        root.path().into(),
        owner,
        server,
    ));
    let snapshot = DraftSnapshot {
        draft_id: "source-draft".into(),
        revision: 7,
        text: "UNSENT_SNAPSHOT_A".into(),
        cursor: 17,
        ..Default::default()
    };
    send.send(UiAction::Send {
        generation: 0,
        now: false,
        snapshot: Some(Box::new(snapshot)),
        draft_id: "source-draft".into(),
        revision: 7,
        thread_id: None,
        cwd: root.path().into(),
        text: "DIFFERENT_SENT_B".into(),
        attachments: vec![],
        connection: None,
        model: None,
        effort: None,
    })
    .await
    .unwrap();
    next_update(&mut incoming, |update| match update {
        UiUpdate::Receipt {
            status: ReceiptStatus::Failed,
            message: Some(reason),
            ..
        } => {
            assert!(reason.contains("does not match"));
            Some(())
        }
        _ => None,
    })
    .await;
    observer
        .store
        .database
        .call(|db| {
            for table in ["receipt", "model_attempt", "draft", "thread"] {
                assert_eq!(
                    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))?,
                    0,
                    "mismatched source must not be accepted or suppressed: {table}"
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    send.send(UiAction::Quit).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(60), bridge)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
