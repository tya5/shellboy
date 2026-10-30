//! Human recovery routes through real frontend/Control, a loopback provider and TestBackend.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    control::Control,
    frontend,
    ipc::LocalServer,
    ui::{
        self, UiAction, UiState, UiUpdate,
        activity::{ActivityPhase, ActivitySnapshot},
    },
};
use std::{sync::mpsc::Receiver, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::timeout,
};

struct Request {
    body: Value,
    reply: oneshot::Sender<(u16, Value)>,
}
async fn proxy() -> (String, mpsc::Receiver<Request>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let body = loop {
                    let mut chunk = [0; 8192];
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() < 1024 * 1024, "fixture request is bounded");
                    if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break serde_json::from_slice(&bytes[end + 4..end + 4 + length])
                                .unwrap();
                        }
                    }
                };
                let (reply, result) = oneshot::channel();
                if send.send(Request { body, reply }).await.is_err() {
                    return;
                }
                let Ok((status, body)) = result.await else {
                    return;
                };
                let body = body.to_string();
                let head = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body.as_bytes()).await;
            });
        }
    });
    (format!("http://{address}/v1"), receive, task)
}
fn enter(ui: &mut UiState) -> Vec<UiAction> {
    ui.event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
}
fn deliver(sender: &ui::actions::ActionSender, actions: Vec<UiAction>) {
    assert!(sender.try_send_batch(actions).is_empty());
}
// The real UI keeps consuming bounded updates while the model is in flight.
async fn request_with_ui(
    requests: &mut mpsc::Receiver<Request>,
    incoming: &Receiver<UiUpdate>,
    ui: &mut UiState,
    sender: &ui::actions::ActionSender,
) -> Request {
    let mut recent = std::collections::VecDeque::new();
    let result = timeout(Duration::from_secs(15), async {
        loop {
            while let Ok(update) = incoming.try_recv() {
                if let UiUpdate::Status { text, .. } = &update {
                    if recent.len() == 8 {
                        recent.pop_front();
                    }
                    recent.push_back(text.clone());
                }
                ui.apply(update);
                deliver(sender, ui.take_deferred_actions());
            }
            match requests.try_recv() {
                Ok(request) => return request,
                Err(mpsc::error::TryRecvError::Disconnected) => panic!("fixture proxy stopped"),
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    result.unwrap_or_else(|_| {
        panic!("missing model request while UI stayed active; recent status: {recent:?}")
    })
}
async fn phase(
    incoming: &Receiver<UiUpdate>,
    ui: &mut UiState,
    sender: &ui::actions::ActionSender,
    wanted: ActivityPhase,
) -> ActivitySnapshot {
    timeout(Duration::from_secs(10), async {
        loop {
            while let Ok(update) = incoming.try_recv() {
                let found = match &update {
                    UiUpdate::Activity { snapshot, .. } if snapshot.phase == wanted => {
                        Some(snapshot.clone())
                    }
                    _ => None,
                };
                ui.apply(update);
                deliver(sender, ui.take_deferred_actions());
                if let Some(snapshot) = found {
                    return snapshot;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
fn screen(ui: &mut UiState) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 28)).unwrap();
    terminal.draw(|frame| ui::draw(frame, ui)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let symbol = buffer[(x, y)].symbol();
            text.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        text.push('\n');
    }
    text
}
#[derive(Clone, Copy, Debug)]
enum RecoveryRoute {
    Retry,
    NewInstruction,
    Stop,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_retry_new_instruction_and_stop_keep_current_targets_and_saved_reasons() {
    for route in [
        RecoveryRoute::Retry,
        RecoveryRoute::NewInstruction,
        RecoveryRoute::Stop,
    ] {
        let root = tempfile::tempdir().unwrap();
        let (url, mut requests, proxy) = proxy().await;
        let config = root.path().join("config.toml");
        std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"{url}\"\nresponse_max_retries=0\n")).unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let mut ui = UiState::new(&control.store.config, root.path().into());
        let (rpc, receive) = mpsc::channel(16);
        let server = LocalServer::bind(&owner, rpc).await.unwrap();
        tokio::spawn(control.clone().serve(receive));
        let (sender, receiver) = ui::actions::channel(64);
        let (updates, incoming) = std::sync::mpsc::sync_channel(128);
        let bridge = tokio::spawn(frontend::run(
            control.clone(),
            receiver,
            updates,
            root.path().into(),
            owner,
            server,
        ));
        ui.insert("original-recovery-request-唯一", false);
        let actions = enter(&mut ui);
        deliver(&sender, actions);
        let first = timeout(Duration::from_secs(10), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            first
                .body
                .to_string()
                .contains("original-recovery-request-唯一")
        );
        first
            .reply
            .send((
                500,
                json!({"error":{"code":"fixture_upstream","message":"fixture recovery failure"}}),
            ))
            .unwrap();
        let failed = phase(&incoming, &mut ui, &sender, ActivityPhase::Recovery).await;
        assert!(screen(&mut ui).contains("fixture recovery failure"));
        let run = failed.run_id.clone().unwrap();
        let turn = failed.turn_id.clone().unwrap();
        ui.apply(UiUpdate::StructuredManagement {
            work_status: false,
            request_id: 999,
            title: "以前の作業".into(),
            omitted: false,
            fields: vec![shellboy::presentation::PresentationField {
                value_omitted: false,
                display: None,
                key: "/run_id".into(),
                label: "Run ID".into(),
                value: "past-reference-run".into(),
            }],
        });
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(screen(&mut ui).contains("fixture recovery failure"));
        ui.insert(
            match route {
                RecoveryRoute::Retry => "/retry",
                RecoveryRoute::NewInstruction => "new-recovery-instruction-追加",
                RecoveryRoute::Stop => "/stop",
            },
            false,
        );
        let actions = enter(&mut ui);
        deliver(&sender, actions);
        match route {
            RecoveryRoute::Stop => {
                let stopped = phase(&incoming, &mut ui, &sender, ActivityPhase::Stopped).await;
                assert_eq!(stopped.run_id.as_deref(), Some(run.as_str()));
                assert!(screen(&mut ui).contains("前回の復旧待ち"));
                assert!(screen(&mut ui).contains("fixture recovery failure"));
                assert!(
                    requests.try_recv().is_err(),
                    "stop must not create another model request"
                );
            }
            RecoveryRoute::Retry | RecoveryRoute::NewInstruction => {
                let mut next = request_with_ui(&mut requests, &incoming, &mut ui, &sender).await;
                let (failure_note, last_reason) = if matches!(route, RecoveryRoute::Retry) {
                    next.reply.send((500,json!({"error":{"code":"fixture_again","message":"second recovery failure"}}))).unwrap();
                    let again = phase(&incoming, &mut ui, &sender, ActivityPhase::Recovery).await;
                    assert_eq!(again.run_id.as_deref(), Some(run.as_str()));
                    assert!(
                        ui.management.is_none(),
                        "first /retry must not open a management report"
                    );
                    ui.insert("/retry", false);
                    let actions = enter(&mut ui);
                    deliver(&sender, actions);
                    next = request_with_ui(&mut requests, &incoming, &mut ui, &sender).await;
                    ("記録したモデル失敗2件", "second recovery failure")
                } else {
                    ("記録したモデル失敗1件", "fixture recovery failure")
                };
                assert!(
                    next.body
                        .to_string()
                        .contains("original-recovery-request-唯一")
                );
                assert_eq!(
                    next.body
                        .to_string()
                        .contains("new-recovery-instruction-追加"),
                    matches!(route, RecoveryRoute::NewInstruction)
                );
                next.reply.send((200,json!({"id":"fixture-recovered","object":"response","status":"completed","output":[{"id":"answer","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"recovered answer","annotations":[]}]}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}))).unwrap();
                let completed = phase(&incoming, &mut ui, &sender, ActivityPhase::Completed).await;
                assert_eq!(completed.run_id.as_deref(), Some(run.as_str()));
                assert_eq!(completed.turn_id.as_deref(), Some(turn.as_str()));
                assert!(completed.recovery.is_none());
                assert!(
                    ui.management.is_none(),
                    "/retry and a new instruction keep the conversation visible"
                );
                timeout(Duration::from_secs(10), async {
                    loop {
                        while let Ok(update) = incoming.try_recv() {
                            ui.apply(update);
                            deliver(&sender, ui.take_deferred_actions());
                        }
                        if ui
                            .view()
                            .nodes()
                            .iter()
                            .any(|node| node.presentation.title.contains(failure_note))
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                let history = screen(&mut ui);
                assert!(history.contains(failure_note));
                assert!(history.contains(last_reason));
            }
        }
        deliver(&sender, vec![UiAction::Quit]);
        timeout(Duration::from_secs(10), async {
            while !bridge.is_finished() {
                while incoming.try_recv().is_ok() {}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        bridge.await.unwrap().unwrap();
        proxy.abort();
    }
}

#[cfg(unix)]
fn ordered_tool_group_visible(nodes: &[ui::UiNode]) -> bool {
    let tools = nodes
        .iter()
        .flat_map(|node| &node.presentation.blocks)
        .filter_map(|block| block.tool.as_ref());
    tools.clone().any(|tool| {
        tool.call_id == "three"
            && tool.ordinal == Some(2)
            && tool.status == "cancelled"
            && tool.command == "printf MUST_NOT_RUN"
    }) && tools.clone().any(|tool| {
        tool.call_id == "two" && tool.status == "running" && tool.reason.contains("背景へ移行")
    }) && nodes
        .iter()
        .filter(|node| node.role == "tool")
        .filter_map(|node| {
            node.presentation
                .blocks
                .iter()
                .find_map(|block| block.tool.as_ref())
                .map(|tool| tool.call_id.as_str())
        })
        .eq(["one", "two", "three"])
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_calls_keep_source_order_numbers_commands_background_and_cancel_reasons() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut requests, proxy) = proxy().await;
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("default_connection=\"fixture\"\ndefault_model=\"fixture-model\"\n[connections.fixture]\nbase_url=\"{url}\"\n")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let mut ui = UiState::new(&control.store.config, root.path().into());
    let (rpc, receive) = mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc).await.unwrap();
    tokio::spawn(control.clone().serve(receive));
    let (sender, receiver) = ui::actions::channel(64);
    let (updates, incoming) = std::sync::mpsc::sync_channel(256);
    let bridge = tokio::spawn(frontend::run(
        control.clone(),
        receiver,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    ui.insert("three ordered tool calls", false);
    let actions = enter(&mut ui);
    deliver(&sender, actions);
    let first = request_with_ui(&mut requests, &incoming, &mut ui, &sender).await;
    let calls=[("one","printf one"),("two","sleep 30"),("three","printf MUST_NOT_RUN")].into_iter().map(|(id,command)|json!({"type":"function_call","id":format!("item-{id}"),"call_id":id,"name":"shell","arguments":json!({"command":command}).to_string()})).collect::<Vec<_>>();
    first
        .reply
        .send((
            200,
            json!({"id":"three-calls","status":"completed","output":calls}),
        ))
        .unwrap();
    // Wait for the second real process; the first result has already been saved.
    timeout(Duration::from_secs(15),async {
        loop {
            while let Ok(update)=incoming.try_recv(){ui.apply(update);deliver(&sender,ui.take_deferred_actions());}
            let running=control.store.database.call(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM tool_call c JOIN shell_execution s ON s.id=c.operation_id WHERE c.call_id='two' AND s.state='running')",[],|row|row.get::<_,bool>(0))?)).await.unwrap();
            if running {break;}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    let thread = ui.view().thread_id.clone().unwrap();
    deliver(
        &sender,
        vec![UiAction::Background {
            thread_id: thread.clone(),
        }],
    );
    let next = request_with_ui(&mut requests, &incoming, &mut ui, &sender).await;
    assert!(next.body.to_string().contains("cancelled"));
    let outputs = next.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| {
            (
                item["call_id"].as_str().unwrap(),
                serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let running = &outputs.iter().find(|(call, _)| *call == "two").unwrap().1;
    let cancelled = &outputs.iter().find(|(call, _)| *call == "three").unwrap().1;
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["executed"], false);
    assert_eq!(
        cancelled["reason"],
        "previous_wait_released_by_human_background"
    );
    assert_eq!(cancelled["prerequisite"]["call_id"], "two");
    assert_eq!(
        cancelled["prerequisite"]["execution_id"],
        running["execution_id"]
    );
    assert_eq!(cancelled["prerequisite"]["state"], "running");
    assert!(cancelled["prerequisite"].get("exit_code").is_none());
    next.reply.send((200,json!({"id":"answer","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"tools returned"}]}]}))).unwrap();
    // Reload the same saved nodes through the frontend, including DB source correlation.
    timeout(Duration::from_secs(15), async {
        loop {
            while let Ok(update) = incoming.try_recv() {
                ui.apply(update);
                deliver(&sender, ui.take_deferred_actions());
            }
            if ordered_tool_group_visible(ui.view().nodes()) {
                break;
            }
            deliver(
                &sender,
                vec![UiAction::LoadHistory {
                    thread_id: thread.clone(),
                    generation: ui.view().generation,
                    before_cursor: None,
                }],
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    // A held prefix of this real history page can contain both released
    // results but not the final cancellation result. RefreshTools observes the
    // latest call facts without changing that prefix's canonical membership.
    let mut early_nodes = ui.view().nodes().to_vec();
    let third_result = early_nodes
        .iter()
        .position(|node| {
            node.role == "tool"
                && node
                    .presentation
                    .blocks
                    .iter()
                    .filter_map(|block| block.tool.as_ref())
                    .any(|tool| tool.call_id == "three")
        })
        .unwrap();
    early_nodes.truncate(third_result);
    let saved_results = control
        .store
        .database
        .call(|db| {
            Ok(db.query_row("SELECT count(*) FROM tool_result", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .await
        .unwrap();
    assert_eq!(saved_results, 3);
    deliver(
        &sender,
        vec![UiAction::RefreshTools {
            thread_id: thread.clone(),
            generation: ui.view().generation,
            revision: u64::MAX,
            nodes: early_nodes,
        }],
    );
    let early_enriched = timeout(Duration::from_secs(15), async {
        loop {
            match incoming.try_recv() {
                Ok(UiUpdate::ToolFacts {
                    revision: u64::MAX,
                    result,
                    ..
                }) => break result.unwrap(),
                Ok(update) => {
                    ui.apply(update);
                    deliver(&sender, ui.take_deferred_actions());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("frontend closed before the controlled observation: {error}"),
            }
        }
    })
    .await
    .unwrap();
    let early_facts = early_enriched
        .iter()
        .flat_map(|node| &node.presentation.blocks)
        .filter_map(|block| block.tool.as_ref())
        .collect::<Vec<_>>();
    assert!(early_facts.iter().any(|tool| tool.call_id == "three"
        && tool.ordinal == Some(2)
        && tool.status == "cancelled"
        && tool.command == "printf MUST_NOT_RUN"));
    assert!(early_facts.iter().any(|tool| tool.call_id == "two"
        && tool.status == "running"
        && tool.reason.contains("背景へ移行")));
    assert_eq!(
        early_enriched
            .iter()
            .filter(|node| node.role == "tool")
            .count(),
        2,
        "the old readiness condition can hold before all canonical result nodes arrive"
    );
    assert!(
        !ordered_tool_group_visible(&early_enriched),
        "live call facts must not certify that every canonical result is loaded"
    );
    let calls = ui
        .view()
        .nodes()
        .iter()
        .flat_map(|node| &node.presentation.blocks)
        .filter(|block| block.kind == shellboy::presentation::BlockKind::ToolCall)
        .filter_map(|block| block.tool.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        calls
            .iter()
            .map(|tool| tool.call_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
    assert_eq!(
        calls.iter().map(|tool| tool.ordinal).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
    assert!(
        calls.iter().any(|tool| tool.call_id == "two"
            && tool.current_state.as_deref() == Some("background_current"))
    );
    let results = ui
        .view()
        .nodes()
        .iter()
        .filter(|node| node.role == "tool")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 3);
    for (index, node) in results.iter().enumerate() {
        let tool = node.presentation.blocks[0].tool.as_ref().unwrap();
        assert_eq!(tool.ordinal, Some(index as u32));
        // Call IDs are item metadata; canonical text is the output leaf only.
        assert!(
            node.presentation
                .fields
                .iter()
                .any(|field| field.key.ends_with("/call_id") && field.value == tool.call_id)
        );
        if let Some(id) = &tool.execution_id {
            assert!(node.text.contains(id));
        }
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 90)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut ui)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut shown = String::new();
    for y in 0..90 {
        let mut x = 0;
        while x < 180 {
            let symbol = buffer[(x, y)].symbol();
            shown.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        shown.push('\n');
    }
    for wanted in [
        "#1 printf one",
        "#2 sleep 30",
        "#3 printf MUST_NOT_RUN",
        "現在 背景で継続中",
        "利用者の操作で背景へ移行",
        "未実行・取消",
        "利用者が先行する呼出しを背景へ移したため未実行",
    ] {
        assert!(shown.contains(wanted), "missing {wanted}: {shown}");
    }
    let count = control
        .store
        .database
        .call(|db| {
            Ok(
                db.query_row("SELECT count(*) FROM shell_execution", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(count, 2, "third call was never spawned");
    deliver(&sender, vec![UiAction::Quit]);
    timeout(Duration::from_secs(15), async {
        while !bridge.is_finished() {
            while incoming.try_recv().is_ok() {}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    bridge.await.unwrap().unwrap();
    proxy.abort();
}
