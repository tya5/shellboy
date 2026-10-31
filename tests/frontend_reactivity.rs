//! Real separate CLI -> owner commit/Changed -> frontend -> drawn current view.
//! Every config, credential/endpoint and DB is temporary; HTTP is loopback-only.
#[path = "support/watchdog.rs"]
mod test_watchdog;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    config::Config,
    control::{Control, EngineEvent},
    frontend,
    ipc::LocalServer,
    protocol::ApiError,
    ui::{UiAction, UiState, UiUpdate},
};
use std::{path::PathBuf, sync::Arc, time::Instant};
use test_watchdog::watchdog;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{broadcast, mpsc, oneshot},
};

struct Fixture {
    root: tempfile::TempDir,
    config: PathBuf,
    data: PathBuf,
    control: Arc<Control>,
    actions: mpsc::Sender<UiAction>,
    updates: mpsc::Receiver<UiUpdate>,
    update_forwarder: std::thread::JoinHandle<()>,
    bridge: tokio::task::JoinHandle<Result<(), ApiError>>,
    ui: UiState,
    terminal: Terminal<TestBackend>,
    source_snapshots: Vec<shellboy::ui::DetailSourceSnapshot>,
    view_sequences: Vec<u64>,
    browser_sequences: Vec<u64>,
    browser_pages: usize,
}
impl Fixture {
    async fn new(url: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        let data = root.path().join("data");
        std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture0'\n[connections.fixture]\nbase_url='{url}'\nresponse_max_retries=0\nresponse_retry_partial=false\n[connections.alternate]\nbase_url='{url}'\nresponse_max_retries=0\n[io]\nself_reply_grace_ms=10\nshutdown_grace_ms=2000\n")).unwrap();
        let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
            .await
            .unwrap();
        let cwd = root.path().to_str().unwrap().to_owned();
        owner.db.call(move |db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('reactive','BASE_TITLE',?1,'','fixture','fixture0','now')",[cwd])?;
            shellboy::history::append_node(&tx,"reactive","paid-baseline",None,"assistant_response",&json!({"text":"PAID_R6_HISTORY remains visible"}),"now")?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let (rpc_tx, rpc_rx) = mpsc::channel(16);
        let server = LocalServer::bind(&owner, rpc_tx).await.unwrap();
        tokio::spawn(control.clone().serve(rpc_rx));
        let (actions, receive) = mpsc::channel(32);
        let (update_send, update_receive) = std::sync::mpsc::sync_channel(512);
        let (forward_send, updates) = mpsc::channel(512);
        // The production terminal receiver is synchronous. A test-owned finite
        // bridge turns real updates into async readiness, without polling time.
        let update_forwarder = std::thread::spawn(move || {
            while let Ok(update) = update_receive.recv() {
                if forward_send.blocking_send(update).is_err() {
                    break;
                }
            }
        });
        let bridge = tokio::spawn(frontend::run(
            control.clone(),
            receive,
            update_send,
            root.path().into(),
            owner,
            server,
        ));
        let mut ui = UiState::new(&Config::default(), root.path().into());
        for action in ui.select_thread("reactive".into()) {
            actions.send(action).await.unwrap();
        }
        let mut fixture = Self {
            root,
            config,
            data,
            control,
            actions,
            updates,
            update_forwarder,
            bridge,
            ui,
            terminal: Terminal::new(TestBackend::new(180, 40)).unwrap(),
            source_snapshots: Vec::new(),
            view_sequences: Vec::new(),
            browser_sequences: Vec::new(),
            browser_pages: 0,
        };
        fixture
            .until("initial selected header", |ui, screen| {
                ui.view().title == "BASE_TITLE" && screen.contains("PAID_R6_HISTORY")
            })
            .await;
        fixture
    }
    fn accept_update(&mut self, update: UiUpdate) {
        match &update {
            UiUpdate::CurrentView { history, .. } => {
                if let UiUpdate::History { read_revision, .. } = history.as_ref() {
                    self.view_sequences.push(*read_revision);
                }
            }
            UiUpdate::BrowserPatch { patch } => self.browser_sequences.push(patch.read_sequence),
            UiUpdate::Threads { .. } => self.browser_pages += 1,
            _ => {}
        }
        if let UiUpdate::DetailLinks {
            result: Ok(snapshot),
            ..
        } = &update
        {
            self.source_snapshots.push(snapshot.clone());
        }
        self.ui.apply(update);
    }
    async fn pump(&mut self) -> String {
        while let Ok(update) = self.updates.try_recv() {
            self.accept_update(update);
        }
        // These are completion/read actions generated by accepted observations,
        // not simulated user input or an extra navigation/refresh operation.
        for action in self.ui.take_deferred_actions() {
            self.actions.send(action).await.unwrap();
        }
        self.terminal
            .draw(|frame| shellboy::ui::draw(frame, &mut self.ui))
            .unwrap();
        // Painting can produce observation/viewport actions. Send them before
        // awaiting the very update that those actions will produce.
        for action in self.ui.take_deferred_actions() {
            self.actions.send(action).await.unwrap();
        }
        let buffer = self.terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..40 {
            let mut x = 0;
            while x < 180 {
                let symbol = buffer[(x, y)].symbol();
                screen.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            screen.push('\n');
        }
        screen
    }
    async fn wait_update_or_action(&mut self) {
        let action_due = self
            .ui
            .deferred_action_wait(Instant::now())
            .map(|delay| tokio::time::Instant::now() + delay);
        tokio::select! {
            update = self.updates.recv() => self.accept_update(update.expect("frontend update channel closed before predicate")),
            _ = async {
                match action_due {
                    Some(due) => tokio::time::sleep_until(due).await,
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    async fn until(
        &mut self,
        label: &str,
        mut predicate: impl FnMut(&UiState, &str) -> bool,
    ) -> String {
        watchdog("frontend saved-state predicate", async {
            loop {
                let screen = self.pump().await;
                if predicate(&self.ui, &screen) {
                    return screen;
                }
                self.wait_update_or_action().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{label}: frontend watchdog; bridge_finished={}",
                self.bridge.is_finished()
            )
        })
    }
    async fn observed_until(
        &mut self,
        label: &str,
        mut predicate: impl FnMut(&Self, &str) -> bool,
    ) -> String {
        watchdog("frontend observation predicate", async {
            loop {
                let screen = self.pump().await;
                if predicate(self, &screen) {
                    return screen;
                }
                self.wait_update_or_action().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{label}: observation watchdog; bridge_finished={}",
                self.bridge.is_finished()
            )
        })
    }
    async fn cli(&self, args: &[&str]) -> (Value, Instant) {
        let mut changed = self.control.subscribe();
        let observed = tokio::spawn(async move { changed_at(&mut changed, false).await });
        let (value, _) = self.cli_only(args).await;
        (value, observed.await.unwrap())
    }
    // GC uses its existing private Notify rather than a Core Changed event.
    // Return invocation start as a conservative bound including process/IPC.
    async fn cli_only(&self, args: &[&str]) -> (Value, Instant) {
        let started = Instant::now();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"));
        command
            .arg("--config")
            .arg(&self.config)
            .arg("--data-dir")
            .arg(&self.data)
            .args(args)
            .current_dir(self.root.path());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("SHELLBOY_") {
                command.env_remove(key);
            }
        }
        let output = watchdog("frontend completion", command.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "CLI {args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["ok"], true, "{value}");
        (value, started)
    }
    async fn finish(mut self) {
        self.actions.send(UiAction::Quit).await.unwrap();
        watchdog("frontend completion", async {
            loop {
                tokio::select! {
                    result = &mut self.bridge => { result.unwrap().unwrap(); break; }
                    Some(_) = self.updates.recv() => {}
                }
            }
        })
        .await
        .expect("frontend shutdown watchdog");
        self.updates.close();
        tokio::task::spawn_blocking(move || self.update_forwarder.join().unwrap())
            .await
            .unwrap();
    }
}
async fn changed_at(events: &mut broadcast::Receiver<EngineEvent>, recovery: bool) -> Instant {
    changed_thread_at(events, "reactive", recovery).await
}
async fn changed_thread_at(
    events: &mut broadcast::Receiver<EngineEvent>,
    expected: &str,
    recovery: bool,
) -> Instant {
    watchdog("frontend completion", async {
        loop {
            if let EngineEvent::Changed {
                thread_id,
                recovery: entered,
            } = events.recv().await.unwrap()
                && thread_id == expected
                && (!recovery || entered)
            {
                return Instant::now();
            }
        }
    })
    .await
    .unwrap()
}
fn latency(route: &str, committed: Instant) {
    let elapsed = committed.elapsed();
    println!(
        "BC_REACTIVITY {route} commit_event_to_paint_us={} target_us=1000000",
        elapsed.as_micros()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_cli_thread_settings_repaint_selected_header_without_input() {
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1").await;
    let cwd = fixture.root.path().join("changed-cwd");
    std::fs::create_dir(&cwd).unwrap();
    let (_, commit) = fixture
        .cli(&[
            "thread",
            "set",
            "--thread",
            "reactive",
            "--title",
            "FOREIGN_TITLE",
            "--connection",
            "alternate",
            "--model",
            "foreign-model",
            "--cwd",
            cwd.to_str().unwrap(),
        ])
        .await;
    fixture
        .until(
            "external settings must repaint without input",
            |ui, screen| {
                let view = ui.view();
                view.title == "FOREIGN_TITLE"
                    && view.connection.as_deref() == Some("alternate")
                    && view.model.as_deref() == Some("foreign-model")
                    && view.cwd == cwd
                    && screen.lines().next().is_some_and(|line| {
                        line.contains("FOREIGN_TITLE")
                            && line.contains("alternate")
                            && line.contains("foreign-model")
                            && line.contains("changed-cwd")
                    })
            },
        )
        .await;
    latency("thread_settings_header", commit);
    fixture.finish().await;
}

struct Incoming {
    reply: oneshot::Sender<()>,
}
async fn failing_proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (send, receive) = mpsc::channel(4);
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 8192];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(at) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let len: usize = String::from_utf8_lossy(&bytes[..at])
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|size| size.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= at + 4 + len {
                            break;
                        }
                    }
                }
                let (reply, gate) = oneshot::channel();
                if send.send(Incoming { reply }).await.is_err() || gate.await.is_err() {
                    return;
                }
                let body = json!({"error":{"code":"fixture_failure","message":"BC_R6_RECOVERY"}})
                    .to_string();
                let response = format!(
                    "HTTP/1.1 500 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, receive, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_cli_queue_cancel_recovery_and_run_stop_repaint_without_input() {
    let (url, mut requests, proxy) = failing_proxy().await;
    let mut fixture = Fixture::new(&url).await;
    let (_, started) = fixture
        .cli(&[
            "message",
            "send",
            "--thread",
            "reactive",
            "--text",
            "FIRST_R6_REQUEST",
        ])
        .await;
    let first = watchdog("frontend completion", requests.recv())
        .await
        .unwrap()
        .unwrap();
    fixture
        .until("model waiting footer", |ui, screen| {
            ui.view().run_id.is_some() && screen.contains("モデル応答待ち")
        })
        .await;
    latency("foreign_first_send_footer", started);
    let (queued, commit) = fixture
        .cli(&[
            "message",
            "send",
            "--thread",
            "reactive",
            "--text",
            "FOREIGN_QUEUED_R6",
        ])
        .await;
    let message = queued["result"]["message_id"].as_str().unwrap().to_owned();
    fixture
        .until("foreign pending input", |ui, screen| {
            ui.view().pending_messages.total == 1
                && ui
                    .view()
                    .pending_messages
                    .items
                    .iter()
                    .any(|pending| pending.message_id == message)
                && screen.contains("FOREIGN_QUEUED_R6")
                && screen.contains("待ち 1")
        })
        .await;
    latency("foreign_queued_input", commit);
    let (_, commit) = fixture
        .cli(&["message", "cancel", "--message", &message])
        .await;
    fixture
        .until("foreign cancellation", |ui, screen| {
            ui.view().pending_messages.total == 0
                && !screen.contains("FOREIGN_QUEUED_R6")
                && !screen.contains("待ち 1")
                && screen.contains("PAID_R6_HISTORY")
        })
        .await;
    latency("foreign_pending_cancel", commit);
    let mut events = fixture.control.subscribe();
    let entered = tokio::spawn(async move { changed_at(&mut events, true).await });
    first.reply.send(()).unwrap();
    let commit = entered.await.unwrap();
    fixture
        .until("saved recovery state/footer", |ui, screen| {
            ui.view().state == "復旧待ち"
                && screen.contains("復旧")
                && screen.contains("PAID_R6_HISTORY")
        })
        .await;
    latency("recovery_footer", commit);
    let run = fixture.ui.view().run_id.clone().unwrap();
    let (_, commit) = fixture.cli(&["run", "stop", "--run", &run]).await;
    fixture
        .until("foreign run stop clears recovery", |ui, screen| {
            ui.view().run_id.is_none()
                && screen.contains('■')
                && screen.contains("前回の復旧待ち")
                && !screen.contains("操作を待っています")
                && !screen.contains("/retry 同じ内容で再試行")
                && screen.contains("PAID_R6_HISTORY")
        })
        .await;
    latency("foreign_run_stop_footer", commit);
    fixture.finish().await;
    proxy.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_cli_raw_gc_updates_open_f6_without_input_or_source_rewrite() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use sha2::{Digest, Sha256};
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1").await;
    let stamp = chrono::Utc::now().to_rfc3339();
    let original=json!({"group_id":"gc-group","item":{"type":"function_call_output","call_id":"gc-call","output":json!({"execution_id":"gc-shell","status":"exited","exit_code":0,"output":"GC_PAID_SOURCE must remain unchanged"}).to_string()}}).to_string();
    let saved = original.clone();
    fixture.control.store.database.call(move |db| {
        let tx=db.transaction()?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('gc-run','reactive','completed',?1,?1)",[&stamp])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('gc-turn','reactive','gc-run','completed',?1,?1)",[&stamp])?;
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('gc-attempt','gc-turn',0,0,'fixture','fixture0',0,'completed','{}',?1)",[&stamp])?;
        tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('gc-group','gc-attempt','gc-turn','completed',?1)",[&stamp])?;
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('gc-shell','gc-run','gc-turn','gc-shell','exited','/','{}','UTF-8','UTF-8','closed',1,0,1,?1,?1)",[&stamp])?;
        tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('gc-group','gc-call',0,'shell','{\"command\":\"printf GC_SOURCE_COMMAND\"}','completed','gc-shell')",[])?;
        tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('gc-shell',0,'stdout',0,?1,?2)",rusqlite::params![vec![b'x';2048],stamp])?;
        let payload:Value=serde_json::from_str(&saved).unwrap();
        shellboy::history::append_node(&tx,"reactive","gc-result",Some("gc-turn"),"tool_result",&payload,&stamp)?;
        tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('gc-group','gc-call','gc-result','exited')",[])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    // A normal external mutation exposes the fixture through the production
    // current-view refresh. Only setup uses direct synthetic database writes.
    fixture
        .cli(&[
            "thread",
            "set",
            "--thread",
            "reactive",
            "--title",
            "GC_FIXTURE_READY",
        ])
        .await;
    fixture
        .until("saved GC tool visible", |ui, screen| {
            ui.view()
                .nodes()
                .iter()
                .any(|node| node.node_id == "gc-result")
                && screen.contains("GC_SOURCE_COMMAND")
        })
        .await;
    for action in fixture
        .ui
        .event(Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)))
    {
        fixture.actions.send(action).await.unwrap();
    }
    let before = fixture
        .until("F6 sees retained output before collection", |_, screen| {
            screen.contains("選択した内容の詳細")
                && screen.contains("gc-result")
                && screen
                    .lines()
                    .any(|line| line.contains("保存出力あり") && line.contains("true"))
                && screen
                    .lines()
                    .any(|line| line.contains("出力保持世代") && line.contains('0'))
        })
        .await;
    assert!(before.contains("gc-call"));
    let first = fixture.source_snapshots.last().unwrap();
    assert_eq!(
        first
            .sources
            .iter()
            .map(|source| source.node_id.as_str())
            .collect::<Vec<_>>(),
        ["gc-result"]
    );
    assert!(
        first.sources[0]
            .fields
            .iter()
            .any(|field| field.key == "/current_tools/0/output_available" && field.value == "true")
    );
    let read_before = first.read_revision;
    // Eligibility setup changes time only. The real collector below must
    // create the retention marker/generation and delete every raw byte itself.
    fixture.control.store.database.call(|db| {
        db.execute("UPDATE shell_execution SET started_at='2000-01-01T00:00:00Z',ended_at='2000-01-01T00:00:01Z' WHERE id='gc-shell'",[])?;Ok(())
    }).await.unwrap();
    let (collected, started) = fixture
        .cli_only(&["storage", "collect", "--category", "raw"])
        .await;
    assert_eq!(collected["result"]["raw_bytes_collected"], 2048);
    // No key/action is sent after opening F6. GC's notification must cause its
    // exact-source read and repaint, retaining the same selected node.
    fixture
        .until("real raw GC must refresh open F6", |_, screen| {
            screen.contains("選択した内容の詳細")
                && screen.contains("gc-result")
                && screen.contains("gc-call")
                && screen
                    .lines()
                    .any(|line| line.contains("保存出力あり") && line.contains("false"))
                && screen
                    .lines()
                    .any(|line| line.contains("出力保持世代") && line.contains('1'))
        })
        .await;
    let elapsed = started.elapsed();
    println!(
        "BC_REACTIVITY raw_gc_open_f6 cli_start_to_paint_us={} target_us=1000000",
        elapsed.as_micros()
    );

    let snapshot = fixture.source_snapshots.last().unwrap();
    assert!(snapshot.read_revision > read_before);
    assert_eq!(snapshot.sources.len(), 1);
    assert_eq!(snapshot.sources[0].node_id, "gc-result");
    for (key, value) in [
        ("/current_tools/0/output_available", "false"),
        ("/current_tools/0/retention_generation", "1"),
    ] {
        assert!(
            snapshot.sources[0]
                .fields
                .iter()
                .any(|field| field.key == key && field.value == value)
        );
    }
    let (source, raw, retention): (String, i64, i64) = fixture
        .control
        .store
        .database
        .call(|db| {
            Ok((
                db.query_row(
                    "SELECT payload_json FROM node WHERE id='gc-result'",
                    [],
                    |row| row.get(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM output_chunk WHERE execution_id='gc-shell'",
                    [],
                    |row| row.get(0),
                )?,
                db.query_row(
                    "SELECT retention_generation FROM shell_execution WHERE id='gc-shell'",
                    [],
                    |row| row.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(source, original);
    assert_eq!(
        Sha256::digest(source.as_bytes()),
        Sha256::digest(original.as_bytes())
    );
    assert_eq!((raw, retention), (0, 1));
    assert!(
        fixture
            .ui
            .view()
            .nodes()
            .iter()
            .any(|node| node.node_id == "paid-baseline" && node.text.contains("PAID_R6_HISTORY"))
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_history_gc_preserves_live_fork_sources_then_invalidates_deleted_child_f7() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1").await;
    const CALL: &str = "f7-shared-call";
    const RESULT: &str = "f7-parent-result";
    const CALL_URL: &str = "https://example.test/CALL_F7_GC";
    const RESULT_URL: &str = "https://example.test/RESULT_F7_GC";
    let stamp = chrono::Utc::now().to_rfc3339();
    let cwd = fixture.root.path().to_str().unwrap().to_owned();
    let call_payload = json!({"group_id":"f7-group","output":[
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("SHARED_F7_PAID_PROSE [call source]({CALL_URL})")}]},
        {"type":"function_call","name":"shell","call_id":"f7-call","arguments":"{\"command\":\"printf F7_SOURCE_COMMAND\"}"}
    ]});
    let result_payload = json!({"group_id":"f7-group","item":{"type":"function_call_output","call_id":"f7-call","output":json!({"status":"exited","exit_code":0,"output":RESULT_URL}).to_string()}});
    let saved_call = call_payload.to_string();
    let saved_result = result_payload.to_string();
    fixture.control.store.database.call(move |db| {
        let tx=db.transaction()?;
        tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('f7-parent','F7_PARENT',?1,'','fixture','fixture0',?2)",rusqlite::params![cwd,stamp])?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('f7-run','f7-parent','completed',?1,?1)",[&stamp])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('f7-turn','f7-parent','f7-run','completed',?1,?1)",[&stamp])?;
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('f7-attempt','f7-turn',0,0,'fixture','fixture0',0,'completed','{}',?1)",[&stamp])?;
        tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('f7-group','f7-attempt','f7-turn','completed',?1)",[&stamp])?;
        shellboy::history::append_node(&tx,"f7-parent","f7-paid-prefix",Some("f7-turn"),"assistant_response",&json!({"text":"PAID_R6_HISTORY shared parent prefix remains in the child"}),&stamp)?;
        shellboy::history::append_node(&tx,"f7-parent",CALL,Some("f7-turn"),"assistant_response",&call_payload,&stamp)?;
        tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('f7-attempt',0,'function_call','completed',?1,?2)",rusqlite::params![json!({"type":"function_call","call_id":"f7-call"}).to_string(),CALL])?;
        tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('f7-group','f7-call',0,'shell','{\"command\":\"printf F7_SOURCE_COMMAND\"}','completed')",[])?;
        shellboy::history::append_node(&tx,"f7-parent",RESULT,Some("f7-turn"),"tool_result",&result_payload,&stamp)?;
        tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('f7-group','f7-call',?1,'exited')",[RESULT])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    // Use the real fork command, through the completed result boundary. A
    // call-only prefix would be rejected by validate_group_boundary and would
    // make an invalid synthetic test rather than exercise a supported route.
    let (forked, _) = fixture
        .cli_only(&["thread", "fork", "--thread", "f7-parent", "--node", RESULT])
        .await;
    let child = forked["result"]["thread_id"].as_str().unwrap().to_owned();
    assert_eq!(forked["result"]["parent_id"], "f7-parent");
    for action in fixture.ui.select_thread(child.clone()) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until("real fork includes complete source pair", |ui, screen| {
            ui.view().thread_id.as_deref() == Some(child.as_str())
                && ui.view().nodes().iter().any(|node| node.node_id == CALL)
                && ui.view().nodes().iter().any(|node| node.node_id == RESULT)
                && screen.contains("F7_SOURCE_COMMAND")
                && screen.contains("PAID_R6_HISTORY")
        })
        .await;
    for action in fixture
        .ui
        .event(Event::Key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE)))
    {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until("F7 hydrates both exact source links", |_, screen| {
            screen.contains("リンク")
                && screen.contains(CALL_URL)
                && screen.contains(RESULT_URL)
                && screen.contains("観測 ")
        })
        .await;
    let first = fixture.source_snapshots.last().unwrap();
    let expected = [CALL, RESULT]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        first
            .sources
            .iter()
            .map(|source| source.node_id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert!(
        first
            .sources
            .iter()
            .all(|source| source.unavailable_reason.is_none())
    );
    let read_before = first.read_revision;
    // No more UI input after F7 opens. Deleting only the parent must not delete
    // the live child, and the child's complete prefix roots BOTH source nodes.
    fixture
        .cli_only(&["thread", "delete", "--thread", "f7-parent"])
        .await;
    fixture
        .cli_only(&["storage", "collect", "--category", "history"])
        .await;
    let protected = fixture.pump().await;
    assert!(
        protected.contains(CALL_URL) && protected.contains(RESULT_URL),
        "live fork source links must survive parent collection: {protected}"
    );
    let expected_call = saved_call.clone();
    let expected_result = saved_result.clone();
    let kept_child = child.clone();
    fixture
        .control
        .store
        .database
        .call(move |db| {
            for (id, payload) in [(CALL, expected_call), (RESULT, expected_result)] {
                let (saved, reason): (Option<String>, Option<String>) = db.query_row(
                    "SELECT payload_json,deleted_reason FROM node WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(saved.as_deref(), Some(payload.as_str()));
                assert!(reason.is_none());
            }
            assert!(db.query_row(
                "SELECT deleted_at IS NULL FROM thread WHERE id=?1",
                [kept_child],
                |row| row.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .await
        .unwrap();
    // Now the final consumer is genuinely deleted. The real history collector
    // must tombstone the sources and its notification must refresh this popup.
    fixture
        .cli_only(&["thread", "delete", "--thread", &child])
        .await;
    let (collected, started) = fixture
        .cli_only(&["storage", "collect", "--category", "history"])
        .await;
    assert!(
        collected["result"]["nodes_tombstoned"].as_u64().unwrap() >= 3,
        "{collected}"
    );
    fixture
        .until(
            "history GC clears unavailable F7 targets without input",
            |ui, screen| {
                ui.view().thread_id.as_deref() == Some(child.as_str())
                    && screen.contains("リンク")
                    && screen.contains("参照元が利用不可")
                    && !screen.contains(CALL_URL)
                    && !screen.contains(RESULT_URL)
            },
        )
        .await;
    let elapsed = started.elapsed();
    println!(
        "BC_REACTIVITY history_gc_open_f7 cli_start_to_paint_us={} target_us=1000000",
        elapsed.as_micros()
    );

    let last = fixture.source_snapshots.last().unwrap();
    assert!(last.read_revision > read_before);
    assert_eq!(
        last.sources
            .iter()
            .map(|source| source.node_id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert!(
        last.sources
            .iter()
            .all(|source| source.unavailable_reason.is_some() && source.links.is_empty())
    );
    fixture
        .control
        .store
        .database
        .call(|db| {
            for id in [CALL, RESULT] {
                let (saved, reason): (Option<String>, Option<String>) = db.query_row(
                    "SELECT payload_json,deleted_reason FROM node WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert!(saved.is_none());
                assert_eq!(reason.as_deref(), Some("thread_deleted_unreachable"));
            }
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        fixture
            .ui
            .view()
            .nodes()
            .iter()
            .any(|node| node.node_id == "f7-paid-prefix" && node.text.contains("PAID_R6_HISTORY")),
        "received display copy is retained even though canonical sources became unavailable"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_header_and_open_browser_share_one_changed_snapshot_without_reload() {
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1").await;
    for action in fixture.ui.event(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    )) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until("browser page", |ui, _| {
            ui.threads_visible() && ui.threads().iter().any(|row| row.thread_id == "reactive")
        })
        .await;
    fixture
        .observed_until("initial browser patch", |fixture, _| {
            !fixture.browser_sequences.is_empty()
        })
        .await;
    let generation = fixture.ui.list_generation();
    let pages = fixture.browser_pages;
    fixture.view_sequences.clear();
    fixture.browser_sequences.clear();
    let (_, commit) = fixture
        .cli(&[
            "thread",
            "set",
            "--thread",
            "reactive",
            "--title",
            "E1_PAIRED_TITLE",
        ])
        .await;
    fixture
        .until("both consumers update without user input", |ui, _| {
            ui.view().title == "E1_PAIRED_TITLE"
                && ui
                    .threads()
                    .iter()
                    .any(|row| row.thread_id == "reactive" && row.title == "E1_PAIRED_TITLE")
        })
        .await;
    latency("browser_paired_header", commit);
    assert_eq!(fixture.ui.list_generation(), generation);
    assert_eq!(
        fixture.browser_pages, pages,
        "normal Changed must not reload the list"
    );
    assert_eq!(fixture.view_sequences.len(), 1, "one selected observation");
    assert_eq!(
        fixture.browser_sequences, fixture.view_sequences,
        "both facts share the DB transaction's read sequence"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_cli_creates_and_deletes_unloaded_browser_rows_without_input_or_reload() {
    let mut fixture = Fixture::new("http://127.0.0.1:9/v1").await;
    for action in fixture.ui.event(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    )) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until("initial browser", |ui, _| {
            ui.threads_visible() && ui.threads().iter().any(|row| row.thread_id == "reactive")
        })
        .await;
    let generation = fixture.ui.list_generation();
    let pages = fixture.browser_pages;
    let (created, _) = fixture.cli_only(&["thread", "create"]).await;
    let id = created["result"]["thread_id"].as_str().unwrap().to_owned();
    let mut events = fixture.control.subscribe();
    let expected = id.clone();
    let changed =
        tokio::spawn(async move { changed_thread_at(&mut events, &expected, false).await });
    fixture
        .cli_only(&[
            "thread",
            "set",
            "--thread",
            &id,
            "--title",
            "E1_FOREIGN_NEW_ROW",
        ])
        .await;
    let commit = changed.await.unwrap();
    fixture
        .until("new row not present in original page", |ui, screen| {
            ui.threads()
                .iter()
                .any(|row| row.thread_id == id && row.title == "E1_FOREIGN_NEW_ROW")
                && screen.contains("E1_FOREIGN_NEW_ROW")
        })
        .await;
    latency("browser_external_add", commit);
    let mut events = fixture.control.subscribe();
    let expected = id.clone();
    let changed =
        tokio::spawn(async move { changed_thread_at(&mut events, &expected, false).await });
    fixture
        .cli_only(&["thread", "delete", "--thread", &id])
        .await;
    let commit = changed.await.unwrap();
    fixture
        .until("external deleted row removed", |ui, screen| {
            ui.threads().iter().all(|row| row.thread_id != id)
                && !screen.contains("E1_FOREIGN_NEW_ROW")
        })
        .await;
    latency("browser_external_delete", commit);
    assert_eq!(fixture.ui.list_generation(), generation);
    assert_eq!(fixture.browser_pages, pages);
    fixture.finish().await;
}
