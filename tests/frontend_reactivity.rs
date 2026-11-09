//! Real separate CLI -> owner commit/Changed -> frontend -> drawn current view.
//! Every config, credential/endpoint and DB is temporary; HTTP is loopback-only.
use crate::test_watchdog::watchdog;
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
    current_view_trace: std::collections::VecDeque<Value>,
    last_screen: String,
    held_initial_views: Option<Vec<UiUpdate>>,
    held_detail_hints: Option<Vec<UiUpdate>>,
    load_failures: usize,
    held_load_failures: Option<Vec<UiUpdate>>,
    detail_trace: std::collections::VecDeque<Value>,
    browser_sequences: Vec<u64>,
    browser_pages: usize,
    browser_trace: std::collections::VecDeque<Value>,
    browser_selected_ready: bool,
    required_browser_row: Option<String>,
    watch_details: Vec<(u64, shellboy::ui::watch::PreparedWatchDetail)>,
    descendant_snapshots: Vec<(u64, Vec<shellboy::ui::watch::DescendantCount>)>,
    index_preview_ready: usize,
    index_preview_failed: usize,
    deletion_preview_responses: usize,
}
impl Fixture {
    async fn new(url: &str) -> Self {
        Self::with_initial_view_hold(url, false).await
    }
    async fn with_initial_view_hold(url: &str, hold: bool) -> Self {
        Self::build(url, hold, false).await
    }
    async fn build(url: &str, hold: bool, long_usage: bool) -> Self {
        Self::build_in(url, hold, long_usage, shellboy::config::UiLanguage::En).await
    }
    async fn build_in(
        url: &str,
        hold: bool,
        long_usage: bool,
        language: shellboy::config::UiLanguage,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        let data = root.path().join("data");
        std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture0'\n[connections.fixture]\nbase_url='{url}'\nresponse_max_retries=0\nresponse_retry_partial=false\n[connections.alternate]\nbase_url='{url}'\nresponse_max_retries=0\n[io]\nself_reply_grace_ms=10\nshutdown_grace_ms=2000\n")).unwrap();
        std::fs::write(
            &config,
            format!(
                "{}\n[indexing]\nauto_kinds=[\"user\",\"summary\",\"assistant_response\"]\n[ui]\nlanguage=\"{}\"\n",
                std::fs::read_to_string(&config).unwrap(),
                if language == shellboy::config::UiLanguage::Ja {
                    "ja"
                } else {
                    "en"
                }
            ),
        )
        .unwrap();
        let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
            .await
            .unwrap();
        let cwd = root.path().to_str().unwrap().to_owned();
        owner.db.call(move |db| {
            let tx=db.transaction()?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('reactive','BASE_TITLE',?1,'','fixture','fixture0','now')",[cwd])?;
            shellboy::history::append_node(&tx,"reactive","paid-baseline",None,"assistant_response",&json!({"text":"PAID_R6_HISTORY remains visible"}),"now")?;
            if long_usage {
                // 90 turns x 40 attempts, newest turns hold the only provider usage.
                tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('lr','reactive','completed','now','now')")?;
                for i in 0..90usize {
                    tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES(?1,'reactive','lr','completed',?2,?2)",rusqlite::params![format!("lt{i:03}"),format!("2026-01-01T00:{:02}:{:02}Z",i/60,i%60)])?;
                    for a in 0..40usize {
                        let usage=(i==89&&a==39).then(||"{\"input_tokens\":159534,\"input_tokens_details\":{\"cached_tokens\":103424}}".to_owned());
                        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES(?1,?2,0,0,'fixture','fixture0',0,'completed','{}',?3,?4)",rusqlite::params![format!("la{i:03}-{a:02}"),format!("lt{i:03}"),usage,format!("2026-01-01T00:00:{a:02}Z")])?;
                    }
                }
            }
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
        let mut config_ui = Config::default();
        config_ui.ui.language = language;
        let mut ui = UiState::new(&config_ui, root.path().into());
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
            current_view_trace: std::collections::VecDeque::new(),
            last_screen: String::new(),
            held_initial_views: hold.then(Vec::new),
            held_detail_hints: None,
            load_failures: 0,
            held_load_failures: None,
            detail_trace: std::collections::VecDeque::new(),
            browser_sequences: Vec::new(),
            browser_pages: 0,
            browser_trace: std::collections::VecDeque::new(),
            browser_selected_ready: false,
            required_browser_row: None,
            watch_details: Vec::new(),
            descendant_snapshots: Vec::new(),
            index_preview_ready: 0,
            index_preview_failed: 0,
            deletion_preview_responses: 0,
        };
        fixture
            .until("initial selected header", |ui, screen| {
                ui.view().title == "BASE_TITLE" && screen.contains("PAID_R6_HISTORY")
            })
            .await;
        fixture
    }
    fn accept_update(&mut self, update: UiUpdate) {
        let trace = match &update {
            UiUpdate::DetailSourcesChanged {
                thread_id,
                generation,
            } => Some(
                json!({"event":"hint_received","thread":thread_id,"generation":generation,"held":self.held_detail_hints.is_some()}),
            ),
            UiUpdate::DetailLinks {
                thread_id,
                generation,
                request_id,
                result,
            } => Some(
                json!({"event":"snapshot_received","thread":thread_id,"generation":generation,"request_id":request_id,"revision":result.as_ref().ok().map(|s|s.read_revision),"sources":result.as_ref().ok().map(|s|s.sources.iter().map(|f|json!({"node":f.node_id,"unavailable":f.unavailable_reason.is_some(),"links":f.links.len()})).collect::<Vec<_>>())}),
            ),
            UiUpdate::LoadFailed {
                thread_id,
                generation,
                message,
                ..
            } => Some(
                json!({"event":"failure_received","thread":thread_id,"generation":generation,"error":message,"held":self.held_load_failures.is_some()}),
            ),
            _ => None,
        };
        if let Some(trace) = trace {
            self.detail_trace.push_back(trace);
            while self.detail_trace.len() > 128 {
                self.detail_trace.pop_front();
            }
        }
        if matches!(&update, UiUpdate::DetailSourcesChanged { .. })
            && let Some(held) = &mut self.held_detail_hints
        {
            assert!(held.len() < 512, "detail hint hold exceeds fixture bound");
            held.push(update);
            return;
        }
        if matches!(&update, UiUpdate::LoadFailed { .. }) {
            if let Some(held) = &mut self.held_load_failures {
                assert!(held.len() < 512, "failure hold exceeds fixture bound");
                held.push(update);
                return;
            }
            self.load_failures += 1;
        }

        if matches!(&update, UiUpdate::CurrentView { .. })
            && let Some(held) = &mut self.held_initial_views
        {
            assert!(
                held.len() < 512,
                "initial-view hold exceeds fixture capacity"
            );
            held.push(update);
            return;
        }
        let index_update = match &update {
            UiUpdate::ScopedManagement { update, .. } => update.as_ref(),
            other => other,
        };
        match index_update {
            UiUpdate::IndexPreviewReady { .. } => self.index_preview_ready += 1,
            UiUpdate::IndexPreviewFailed { .. } => self.index_preview_failed += 1,
            UiUpdate::ThreadDeletion { execute: false, .. } => self.deletion_preview_responses += 1,
            _ => {}
        }
        match &update {
            UiUpdate::CurrentView {
                generation,
                details,
                context_usage,
                history,
                ..
            } => {
                if let UiUpdate::History {
                    read_revision,
                    selection_revision,
                    ..
                } = history.as_ref()
                {
                    self.view_sequences.push(*read_revision);
                    self.current_view_trace.push_back(json!({"thread":details.thread_id,"title":details.title,"generation":generation,"read_revision":read_revision,"selection_revision":selection_revision,"input_tokens":context_usage.input_tokens,"cached_tokens":context_usage.cached_tokens,"output_tokens":context_usage.output_tokens,"ui_generation_before":self.ui.view().generation,"ui_read_revision_before":self.ui.view().delivery_read_revision,"ui_selection_revision_before":self.ui.view().selection_revision}));
                    while self.current_view_trace.len() > 8 {
                        self.current_view_trace.pop_front();
                    }
                }
            }
            UiUpdate::BrowserPatch { patch } => {
                self.browser_sequences.push(patch.read_sequence);
                self.browser_trace.push_back(json!({"kind":"patch","generation":patch.generation,"sequence":patch.read_sequence,"rows":patch.rows.iter().take(8).map(|row|json!({"id":row.thread_id,"state":row.state})).collect::<Vec<_>>(),"missing":patch.missing_ids.iter().take(8).collect::<Vec<_>>()}));
                while self.browser_trace.len() > 32 {
                    self.browser_trace.pop_front();
                }
                self.browser_selected_ready |= patch
                    .excerpt
                    .as_ref()
                    .is_some_and(|(id, value)| id == "reactive" && value.is_ok());
            }
            UiUpdate::Threads { .. } => {
                self.browser_pages += 1;
                self.browser_trace
                    .push_back(json!({"kind":"page","number":self.browser_pages}));
                while self.browser_trace.len() > 32 {
                    self.browser_trace.pop_front();
                }
            }
            UiUpdate::WatchDetails {
                read_sequence,
                details,
                ..
            } => self.watch_details.extend(
                details
                    .iter()
                    .cloned()
                    .map(|detail| (*read_sequence, detail)),
            ),
            UiUpdate::Descendants {
                read_sequence,
                counts,
                error: None,
                ..
            } => self
                .descendant_snapshots
                .push((*read_sequence, counts.clone())),
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
        if let Some(id) = &self.required_browser_row {
            assert!(
                self.ui.threads().iter().any(|row| &row.thread_id == id),
                "open browser lost an existing unfiltered row after an individual update: {id}"
            );
        }
    }
    async fn pump(&mut self) -> String {
        while let Ok(update) = self.updates.try_recv() {
            self.accept_update(update);
        }
        // These are completion/read actions generated by accepted observations,
        // not simulated user input or an extra navigation/refresh operation.
        for action in self.ui.take_deferred_actions() {
            if let UiAction::LoadDetailLinks {
                thread_id,
                generation,
                request_id,
                ..
            } = &action
            {
                self.detail_trace.push_back(json!({"event":"snapshot_requested","thread":thread_id,"generation":generation,"request_id":request_id}));
                while self.detail_trace.len() > 128 {
                    self.detail_trace.pop_front();
                }
            }
            self.actions.send(action).await.unwrap();
        }
        // Read the completed frame, not TestBackend's diff-applied cells:
        // TestBackend does not clear a wide glyph's trailing cell when a later
        // diff skips it, so stale halves could appear in the text read below.
        let buffer = self
            .terminal
            .draw(|frame| shellboy::ui::draw(frame, &mut self.ui))
            .unwrap()
            .buffer
            .clone();
        // Painting can produce observation/viewport actions. Send them before
        // awaiting the very update that those actions will produce.
        for action in self.ui.take_deferred_actions() {
            self.actions.send(action).await.unwrap();
        }
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
        self.last_screen = screen.clone();
        screen
    }
    async fn wait_update_or_action(&mut self) {
        let action_delay = self.ui.deferred_action_wait(Instant::now());
        tokio::select! {
            update = self.updates.recv() => self.accept_update(update.expect("frontend update channel closed before predicate")),
            _ = async {
                match action_delay {
                    Some(delay) => tokio::time::sleep(delay).await,
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
        let result = watchdog("frontend saved-state predicate", async {
            loop {
                let screen = self.pump().await;
                if predicate(&self.ui, &screen) {
                    return screen;
                }
                self.wait_update_or_action().await;
            }
        })
        .await;
        match result {
            Ok(screen) => screen,
            Err(_) => {
                let saved=watchdog("frontend failure saved-state diagnostics", self.control.store.database.call(|db|{
                    let mut q=db.prepare("SELECT json_object('thread',r.thread_id,'run',r.id,'state',r.state,'turn_state',u.state,'attempt_state',a.state,'failure_kind',CASE WHEN json_valid(a.error_json) THEN json_extract(a.error_json,'$.error.kind') END,'generation',u.generation) FROM run r LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=r.id ORDER BY rowid DESC LIMIT 1) LEFT JOIN model_attempt a ON a.rowid=(SELECT rowid FROM model_attempt WHERE turn_id=u.id ORDER BY rowid DESC LIMIT 1) ORDER BY r.rowid DESC LIMIT 8")?;
                    q.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>().map_err(Into::into)
                })).await;
                let rows = self
                    .ui
                    .threads()
                    .iter()
                    .take(8)
                    .map(|row| (&row.thread_id, &row.state))
                    .collect::<Vec<_>>();
                panic!(
                    "{label}: frontend watchdog; bridge_finished={}; ui_rows={rows:?}; saved={saved:?}; browser_generation={}; pages={}; browser_trace={:?}; current_view_snapshots={:?}; detail_trace={:?}\nLAST DRAWN SCREEN:\n{}",
                    self.bridge.is_finished(),
                    self.ui.list_generation(),
                    self.browser_pages,
                    self.browser_trace,
                    self.current_view_trace,
                    self.detail_trace,
                    self.last_screen
                )
            }
        }
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
                "{label}: observation watchdog; bridge_finished={}; held_current_views={}; browser_trace={:?}; current_view_snapshots={:?}; detail_trace={:?}\nLAST DRAWN SCREEN:\n{}",
                self.bridge.is_finished(), self.held_initial_views.as_ref().map_or(0, Vec::len), self.browser_trace, self.current_view_trace, self.detail_trace, self.last_screen
            )
        })
    }
    async fn command(&mut self, text: &str) {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        // Pasted slash text is deliberately literal user content. Type a real
        // command here, rather than asking a model to execute its quoted text.
        for event in text
            .chars()
            .map(|ch| Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)))
            .chain(std::iter::once(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
        {
            for action in self.ui.event(event) {
                self.actions.send(action).await.unwrap();
            }
        }
    }
    async fn cli_thread(&self, thread: &str, args: &[&str]) -> (Value, Instant) {
        let mut events = self.control.subscribe();
        let thread = thread.to_owned();
        let observed =
            tokio::spawn(async move { changed_thread_at(&mut events, &thread, false).await });
        let (value, _) = self.cli_only(args).await;
        (value, observed.await.unwrap())
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
        let binary = std::env::var_os("SHELLBOY_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_shellboy").into());
        let mut command = tokio::process::Command::new(binary);
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
    let mut fixture = Fixture::build_in(&url, false, false, shellboy::config::UiLanguage::Ja).await;
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
                && screen
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim_start().starts_with('#'))
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
    let mut fixture = Fixture::build_in(
        "http://127.0.0.1:9/v1",
        false,
        false,
        shellboy::config::UiLanguage::Ja,
    )
    .await;
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
    history_gc_f7_ordering(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_history_gc_f7_last_hint_precedes_deleted_thread_failure() {
    history_gc_f7_ordering(true).await;
}

async fn history_gc_f7_ordering(hint_first: bool) {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut fixture = Fixture::build_in(
        "http://127.0.0.1:9/v1",
        false,
        false,
        shellboy::config::UiLanguage::Ja,
    )
    .await;
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
    let failures_before_delete = fixture.load_failures;
    if hint_first {
        fixture.held_load_failures = Some(Vec::new());
    } else {
        fixture.held_detail_hints = Some(Vec::new());
    }
    // Now the final consumer is genuinely deleted. The real history collector
    // must tombstone the sources and its notification must refresh this popup.
    fixture
        .cli_only(&["thread", "delete", "--thread", &child])
        .await;
    // The deletion signal starts the maintenance pass at once (#171), so it
    // may tombstone the sources before this explicit collect does. Either
    // way the committed tombstones must refresh the open popup.
    let (collected, started) = fixture
        .cli_only(&["storage", "collect", "--category", "history"])
        .await;
    assert_eq!(collected["ok"], true, "{collected}");
    if hint_first {
        watchdog("last GC hint before deleted Thread failure", async {
            loop {
                fixture.pump().await;
                let collected = fixture.source_snapshots.last().is_some_and(|snapshot| {
                    snapshot
                        .sources
                        .iter()
                        .any(|source| source.unavailable_reason.is_some())
                });
                if collected
                    && fixture
                        .held_load_failures
                        .as_ref()
                        .is_some_and(|updates| !updates.is_empty())
                {
                    break;
                }
                fixture.wait_update_or_action().await;
            }
        })
        .await
        .unwrap();
        for failure in fixture.held_load_failures.take().unwrap() {
            fixture.accept_update(failure);
        }
    } else {
        watchdog("GC hint and deleted Thread failure barrier", async {
            loop {
                fixture.pump().await;
                if fixture.load_failures > failures_before_delete
                    && fixture
                        .held_detail_hints
                        .as_ref()
                        .is_some_and(|hints| !hints.is_empty())
                {
                    break;
                }
                fixture.wait_update_or_action().await;
            }
        })
        .await
        .unwrap();
        for hint in fixture.held_detail_hints.take().unwrap() {
            fixture.accept_update(hint);
        }
    }
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
    println!(
        "F7_ORDERING_TRACE hint_first={hint_first} {}",
        serde_json::to_string(&fixture.detail_trace).unwrap()
    );
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

fn initial_selected_browser_ready(current_views: &[u64], browser_selected_ready: bool) -> bool {
    // Thread + History can paint before the initial CurrentView refresh arrives.
    // Do not count that pending setup observation as the first Changed refresh.
    browser_selected_ready && !current_views.is_empty()
}

#[test]
fn initial_browser_readiness_waits_for_current_view_in_either_delivery_order() {
    assert!(!initial_selected_browser_ready(&[], false));
    assert!(!initial_selected_browser_ready(&[], true));
    assert!(!initial_selected_browser_ready(&[1], false));
    assert!(initial_selected_browser_ready(&[1], true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initial_browser_excerpt_does_not_complete_a_delayed_selected_view() {
    let mut fixture = Fixture::with_initial_view_hold("http://127.0.0.1:9/v1", true).await;
    for action in fixture.ui.event(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    )) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .observed_until("initial browser excerpt", |fixture, _| {
            fixture.browser_selected_ready
        })
        .await;
    // Idle activity can stop startup polling before any CurrentView is read.
    // A committed Changed event guarantees the observation whose delivery we hold.
    const BASELINE: &str = "DELAYED_SELECTED_BASELINE";
    fixture
        .cli(&["thread", "set", "--thread", "reactive", "--title", BASELINE])
        .await;
    fixture
        .observed_until(
            "committed selected baseline is held while browser adopts it",
            |fixture, _| {
                fixture
                    .ui
                    .threads()
                    .iter()
                    .any(|row| row.thread_id == "reactive" && row.title == BASELINE)
                    && fixture.held_initial_views.as_ref().is_some_and(|held| {
                        held.iter().any(|update| {
                            matches!(update,
                                UiUpdate::CurrentView { details, .. }
                                    if details.thread_id == "reactive" && details.title == BASELINE
                            )
                        })
                    })
            },
        )
        .await;
    assert_eq!(fixture.ui.view().title, "BASE_TITLE");
    assert!(fixture.view_sequences.is_empty());
    assert!(!initial_selected_browser_ready(
        &fixture.view_sequences,
        fixture.browser_selected_ready,
    ));
    // Release actual owner observations, not a sleep or a synthetic refresh.
    for update in fixture.held_initial_views.take().unwrap() {
        fixture.accept_update(update);
    }
    fixture
        .observed_until("both baseline consumers have adopted", |fixture, _| {
            initial_selected_browser_ready(&fixture.view_sequences, fixture.browser_selected_ready)
                && fixture.ui.view().title == BASELINE
                && fixture
                    .ui
                    .threads()
                    .iter()
                    .any(|row| row.thread_id == "reactive" && row.title == BASELINE)
        })
        .await;
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
    // Header/history and even a browser excerpt do not guarantee a startup
    // CurrentView. Establish a saved baseline before clearing measured vectors.
    const BASELINE: &str = "PAIRED_SELECTED_BASELINE";
    fixture
        .cli(&["thread", "set", "--thread", "reactive", "--title", BASELINE])
        .await;
    fixture
        .observed_until("committed selected and browser baseline", |fixture, _| {
            initial_selected_browser_ready(&fixture.view_sequences, fixture.browser_selected_ready)
                && fixture.current_view_trace.back().is_some_and(|snapshot| {
                    snapshot["thread"] == "reactive" && snapshot["title"] == BASELINE
                })
                && fixture.ui.view().title == BASELINE
                && fixture
                    .ui
                    .threads()
                    .iter()
                    .any(|row| row.thread_id == "reactive" && row.title == BASELINE)
        })
        .await;
    let generation = fixture.ui.list_generation();
    let pages = fixture.browser_pages;
    watchdog("consecutive shared read deadlines", async {
        for step in 0..16 {
            let title = format!("E1_PAIRED_TITLE_{step}");
            fixture.view_sequences.clear();
            fixture.browser_sequences.clear();
            let (_, commit) = fixture
                .cli(&["thread", "set", "--thread", "reactive", "--title", &title])
                .await;
            fixture
                .until("both consumers update without user input", |ui, _| {
                    ui.view().title == title
                        && ui
                            .threads()
                            .iter()
                            .any(|row| row.thread_id == "reactive" && row.title == title)
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
                "step {step}: both facts share the DB transaction's read sequence"
            );
        }
    })
    .await
    .unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scoped_watch_foreign_cli_updates_parent_counts_and_rejects_delayed_retry_confirmation() {
    watchdog("scoped Watch CLI lifecycle and stale confirmation", async {

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use shellboy::activity::ActivityPhase;
    let (url, mut requests, proxy) = failing_proxy().await;
    let mut fixture = Fixture::new(&url).await;
    let (forked, _) = fixture
        .cli_only(&[
            "thread",
            "fork",
            "--thread",
            "reactive",
            "--node",
            "paid-baseline",
        ])
        .await;
    let child = forked["result"]["thread_id"].as_str().unwrap().to_owned();
    fixture
        .cli_only(&[
            "thread",
            "set",
            "--thread",
            &child,
            "--title",
            "F_SCOPED_UNOWNED_CHILD",
        ])
        .await;
    let prior_pages = fixture.browser_pages;
    fixture.command("/threads descendants").await;
    fixture
        .observed_until("scoped Watch opens actual candidate page", |f, screen| {
            f.ui.threads_visible() && screen.contains("Descendant Watch") && f.browser_pages > prior_pages
        })
        .await;
    let generation = fixture.ui.list_generation();
    let pages = fixture.browser_pages;
    let (sent, commit) = fixture
        .cli_thread(
            &child,
            &[
                "message",
                "send",
                "--thread",
                &child,
                "--text",
                "F_UNOWNED_CHILD_REQUEST",
            ],
        )
        .await;
    let run = sent["result"]["run_id"].as_str().unwrap().to_owned();
    let first = watchdog("controlled proxy request", requests.recv())
        .await
        .unwrap()
        .unwrap();
    fixture
        .observed_until(
            "new unowned descendant appears without input",
            |f, screen| {
                screen.contains("F_SCOPED_UNOWNED_CHILD")
                    && f.descendant_snapshots.last().is_some_and(|(_, counts)| {
                        counts.iter().any(|count| {
                            count.root == "reactive" && count.active == 1 && count.recovery == 0
                        })
                    })
            },
        )
        .await;
    latency("watch_new_unowned_descendant", commit);
    fixture
        .observed_until("painted child gets bounded real details", |f, _| {
            f.watch_details.iter().rev().any(|(_, detail)| {
                detail.facts().thread_id == child
                    && detail.facts().phase == ActivityPhase::AwaitingModel
            })
        })
        .await;
    let (_, commit) = fixture
        .cli(&[
            "thread",
            "set",
            "--thread",
            "reactive",
            "--title",
            "F_PARENT_TITLE_CHANGED",
        ])
        .await;
    fixture
        .observed_until("parent-only dependency refreshes visible child", |f, _| {
            f.ui.view().title == "F_PARENT_TITLE_CHANGED"
                && f.watch_details
                    .iter()
                    .rev()
                    .find(|(_, detail)| detail.facts().thread_id == child)
                    .is_some_and(|(_, detail)| {
                        detail.facts().parent_title.as_deref() == Some("F_PARENT_TITLE_CHANGED")
                    })
        })
        .await;
    latency("watch_parent_title_dependency", commit);
    fixture.view_sequences.clear();
    fixture.browser_sequences.clear();
    fixture.watch_details.clear();
    fixture.descendant_snapshots.clear();
    let mut events = fixture.control.subscribe();
    let target = child.clone();
    let recovery = tokio::spawn(async move { changed_thread_at(&mut events, &target, true).await });
    first.reply.send(()).unwrap();
    let commit = recovery.await.unwrap();
    fixture
        .observed_until(
            "same read updates Watch recovery and parent counts",
            |f, _| {
                f.descendant_snapshots.last().is_some_and(|(_, counts)| {
                    counts.iter().any(|count| {
                        count.root == "reactive" && count.active == 0 && count.recovery == 1
                    })
                }) && f
                    .watch_details
                    .iter()
                    .rev()
                    .find(|(_, detail)| detail.facts().thread_id == child)
                    .is_some_and(|(_, detail)| detail.facts().phase == ActivityPhase::Recovery)
            },
        )
        .await;
    latency("watch_recovery_and_parent_counts", commit);
    let count_sequence = fixture.descendant_snapshots.last().unwrap().0;
    assert!(
        fixture.browser_sequences.contains(&count_sequence),
        "metadata and counts share one transaction"
    );
    assert!(
        fixture
            .watch_details
            .iter()
            .any(|(seq, detail)| *seq == count_sequence && detail.facts().thread_id == child),
        "visible details share the count snapshot"
    );
    assert!(
        fixture.view_sequences.is_empty(),
        "an unowned child change must not reload the parent's conversation"
    );
    assert_eq!(fixture.ui.list_generation(), generation);
    assert_eq!(
        fixture.browser_pages, pages,
        "ordinary changes must not reload candidates"
    );
    let old_generation = fixture
        .watch_details
        .last()
        .unwrap()
        .1
        .facts()
        .turn_generation
        .unwrap();
    for action in fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::ALT,
    ))) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until("exact retry confirmation", |_, screen| {
            screen.contains("Retry the displayed saved target?") && screen.contains(&run)
        })
        .await;
    // Capture a genuinely confirmed UI packet, then delay delivery while another
    // CLI changes the same Turn. This exercises the DB guard, not a forged ID.
    let confirmed = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::NONE,
    )));
    assert!(
        matches!(confirmed.as_slice(),[UiAction::WatchAction(target)] if target.turn_generation==Some(old_generation))
    );
    fixture.cli_only(&["run", "retry", "--run", &run]).await;
    let second = watchdog("controlled proxy request", requests.recv())
        .await
        .unwrap()
        .unwrap();
    let mut events = fixture.control.subscribe();
    let target = child.clone();
    let recovered =
        tokio::spawn(async move { changed_thread_at(&mut events, &target, true).await });
    second.reply.send(()).unwrap();
    recovered.await.unwrap();
    fixture
        .observed_until("new recovery generation is visible", |f, _| {
            f.watch_details
                .iter()
                .rev()
                .find(|(_, detail)| detail.facts().thread_id == child)
                .is_some_and(|(_, detail)| {
                    detail.facts().phase == ActivityPhase::Recovery
                        && detail
                            .facts()
                            .turn_generation
                            .is_some_and(|generation| generation > old_generation)
                })
        })
        .await;
    for action in confirmed {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until(
            "delayed confirmation fails without retrying replacement work",
            |_, screen| screen.contains("Watch | Retry not applied |"),
        )
        .await;
    let saved_run = run.clone();
    let (turn_generation,attempts):(u64,u64)=fixture.control.store.database.call(move|db|{
        Ok((db.query_row("SELECT generation FROM turn WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[&saved_run],|row|row.get::<_,i64>(0))? as u64,
            db.query_row("SELECT count(*) FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.run_id=?1",[saved_run],|row|row.get::<_,i64>(0))? as u64))
    }).await.unwrap();
    assert_eq!(turn_generation, old_generation + 1);
    assert_eq!(
        attempts, 2,
        "stale confirmation must not create a third model attempt"
    );
    assert!(requests.try_recv().is_err());
    fixture.cli_only(&["run", "stop", "--run", &run]).await;
    fixture
        .observed_until("terminal unowned descendant count settles", |f, _| {
            f.descendant_snapshots.last().is_some_and(|(_, counts)| {
                counts.iter().any(|count| {
                    count.root == "reactive"
                        && count.available
                        && count.active == 0
                        && count.recovery == 0
                })
            })
        })
        .await;
    fixture.finish().await;
    proxy.abort();

    }).await.expect("scoped Watch lifecycle watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_list_keeps_running_foreign_row_through_repeated_cli_reordering() {
    watchdog("running row remains in open List", async {
        let (url, mut requests, proxy) = failing_proxy().await;
        let mut fixture = Fixture::new(&url).await;
        let (worker, _) = fixture.cli_only(&["thread", "create"]).await;
        let worker = worker["result"]["thread_id"].as_str().unwrap().to_owned();
        let (other, _) = fixture.cli_only(&["thread", "create"]).await;
        let other = other["result"]["thread_id"].as_str().unwrap().to_owned();
        fixture.command("/threads").await;
        fixture
            .until("both foreign rows loaded", |ui, _| {
                ui.threads_visible()
                    && [&worker, &other]
                        .iter()
                        .all(|id| ui.threads().iter().any(|row| &row.thread_id == *id))
            })
            .await;
        fixture.required_browser_row = Some(worker.clone());
        let generation = fixture.ui.list_generation();
        let pages = fixture.browser_pages;
        fixture
            .cli_thread(
                &worker,
                &[
                    "message",
                    "send",
                    "--thread",
                    &worker,
                    "--text",
                    "SYNTHETIC_RUNNING_ROW",
                ],
            )
            .await;
        let request = watchdog("local held model request", requests.recv())
            .await
            .unwrap()
            .unwrap();
        fixture
            .until("foreign worker running", |ui, _| {
                ui.threads()
                    .iter()
                    .any(|row| row.thread_id == worker && row.state == "running")
            })
            .await;
        for step in 0..16 {
            // Advance a competing row first, then the running row: both orders
            // move while the same unfiltered browser generation remains open.
            let other_title = format!("OTHER_ROW_{step}");
            fixture
                .cli_thread(
                    &other,
                    &["thread", "set", "--thread", &other, "--title", &other_title],
                )
                .await;
            fixture
                .until("competing row advances", |ui, _| {
                    ui.threads()
                        .iter()
                        .any(|row| row.thread_id == other && row.title == other_title)
                })
                .await;
            let title = format!("RUNNING_ROW_{step}");
            fixture
                .cli_thread(
                    &worker,
                    &["thread", "set", "--thread", &worker, "--title", &title],
                )
                .await;
            fixture
                .until("running row advances", |ui, _| {
                    ui.threads().iter().any(|row| {
                        row.thread_id == worker && row.title == title && row.state == "running"
                    })
                })
                .await;
            assert_eq!(fixture.ui.list_generation(), generation);
            assert_eq!(
                fixture.browser_pages, pages,
                "no reopen/full reload masks disappearance"
            );
        }
        drop(request);
        fixture.finish().await;
        proxy.abort();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_browser_keeps_reordered_running_rows_visible_without_reopen() {
    let (url, mut requests, proxy) = failing_proxy().await;
    let mut fixture = Fixture::new(&url).await;
    let (created, _) = fixture.cli_only(&["thread", "create"]).await;
    let worker = created["result"]["thread_id"].as_str().unwrap().to_owned();
    fixture
        .cli_only(&[
            "thread",
            "set",
            "--thread",
            &worker,
            "--title",
            "WORKER_ONE",
        ])
        .await;
    let (other, _) = fixture.cli_only(&["thread", "create"]).await;
    let other = other["result"]["thread_id"].as_str().unwrap().to_owned();
    fixture
        .cli_only(&[
            "thread",
            "set",
            "--thread",
            &other,
            "--title",
            "WORKER_THREE",
        ])
        .await;
    fixture
        .cli(&[
            "thread",
            "set",
            "--thread",
            "reactive",
            "--title",
            "SELECTED_STAYS",
        ])
        .await;
    fixture
        .until("selected title before browser", |ui, _| {
            ui.view().title == "SELECTED_STAYS"
        })
        .await;
    for action in fixture.ui.event(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('t'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    )) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until("all three initial rows", |ui, screen| {
            ui.threads()
                .iter()
                .filter(|row| !row.thread_id.starts_with("draft:"))
                .count()
                == 3
                && screen.contains("WORKER_ONE")
                && screen.contains("WORKER_THREE")
        })
        .await;
    let generation = fixture.ui.list_generation();
    let pages = fixture.browser_pages;
    let witness = fixture.root.path().join("browser-witness.jsonl");
    std::fs::write(&witness, "").unwrap();
    for cycle in 0..4 {
        let worker = if cycle % 2 == 0 { &worker } else { &other };
        let (accepted, _) = fixture
            .cli_only(&[
                "message",
                "send",
                "--thread",
                worker,
                "--text",
                "Synthetic running work",
            ])
            .await;
        let run = accepted["result"]["run_id"].as_str().unwrap().to_owned();
        let incoming = watchdog("held model request", requests.recv())
            .await
            .unwrap()
            .unwrap();
        let screen = fixture
            .until(
                &format!("running row facts cycle={cycle} worker={worker}"),
                |ui, _| {
                    ui.threads().iter().any(|row| {
                        row.thread_id.as_str() == worker.as_str() && row.state == "running"
                    })
                },
            )
            .await;
        let (canonical, _) = fixture.cli_only(&["thread", "list"]).await;
        assert!(
            canonical["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["thread_id"].as_str() == Some(worker.as_str())),
            "saved Thread still exists while the browser is open"
        );
        let visible_before_reopen = screen.contains("WORKER_ONE");
        if !visible_before_reopen {
            for code in [
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyCode::Char('t'),
            ] {
                let modifiers = if code == crossterm::event::KeyCode::Esc {
                    crossterm::event::KeyModifiers::NONE
                } else {
                    crossterm::event::KeyModifiers::CONTROL
                };
                for action in fixture.ui.event(crossterm::event::Event::Key(
                    crossterm::event::KeyEvent::new(code, modifiers),
                )) {
                    fixture.actions.send(action).await.unwrap();
                }
            }
            fixture
                .until("reopen restores row", |_, screen| {
                    screen.contains("WORKER_ONE") && screen.contains("WORKER_THREE")
                })
                .await;
        }
        assert!(
            visible_before_reopen,
            "cycle {cycle}: canonical row remains; reopening restored it, but an in-place reorder hid it: {screen}"
        );
        assert!(screen.contains("WORKER_THREE"));
        {
            use std::io::Write;
            writeln!(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&witness)
                    .unwrap(),
                "{}",
                json!({"cycle":cycle,"state":"running","present":true})
            )
            .unwrap();
        }
        fixture.cli_only(&["run", "stop", "--run", &run]).await;
        let _ = incoming.reply.send(());
        let screen = fixture
            .until("stopped row facts", |ui, _| {
                ui.threads()
                    .iter()
                    .any(|row| row.thread_id.as_str() == worker.as_str() && row.state == "stopped")
            })
            .await;
        assert!(
            screen.contains("WORKER_ONE"),
            "stopped row still belongs to the browser"
        );
        {
            use std::io::Write;
            writeln!(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&witness)
                    .unwrap(),
                "{}",
                json!({"cycle":cycle,"state":"stopped","present":true})
            )
            .unwrap();
        }
        assert_eq!(fixture.ui.list_generation(), generation);
        assert_eq!(
            fixture.browser_pages, pages,
            "no repair reload should be needed"
        );
    }
    assert_eq!(std::fs::read_to_string(witness).unwrap().lines().count(), 8);
    fixture.finish().await;
    proxy.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_observation_retains_success_after_one_failed_explicit_refresh() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    // A hard limit ensures an accidental refresh loop cannot proliferate work.
    let proxy = tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
                bytes.push(byte[0]);
                assert!(bytes.len() < 16384);
            }
            assert!(String::from_utf8_lossy(&bytes).starts_with("GET /v1/models "));
            let (status, body) = if attempt == 0 {
                ("200 OK", json!({"data":[{"id":"retained-model"}]}))
            } else {
                (
                    "500 Fixture",
                    json!({"error":{"message":"synthetic failure"}}),
                )
            };
            let body = body.to_string();
            stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let mut fixture = Fixture::build_in(&url, false, false, shellboy::config::UiLanguage::Ja).await;
    let mut config = Config::parse(&std::fs::read_to_string(&fixture.config).unwrap()).unwrap();
    config.ui.language = shellboy::config::UiLanguage::Ja;
    fixture.ui = UiState::new(&config, fixture.root.path().into());
    for action in fixture.ui.select_thread("reactive".into()) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture.command("/models fixture").await;
    let shown = fixture
        .until("first model list", |_, screen| {
            screen.contains("retained-model")
        })
        .await;
    assert_eq!(shown.matches("retained-model").count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for action in fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    ))) {
        fixture.actions.send(action).await.unwrap();
    }
    let shown = fixture
        .until("failed model refresh", |_, screen| {
            screen.contains("再取得に失敗")
        })
        .await;
    assert_eq!(shown.matches("retained-model").count(), 1);
    assert_eq!(shown.matches("再取得に失敗").count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let actions = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert!(
        actions.is_empty(),
        "failed refresh must not confirm an old model"
    );
    watchdog("bounded model proxy", proxy)
        .await
        .unwrap()
        .unwrap();
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn index_observation_retains_success_after_one_failed_explicit_refresh() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut fixture = Fixture::build_in(
        "http://127.0.0.1:9/v1",
        false,
        false,
        shellboy::config::UiLanguage::Ja,
    )
    .await;
    fixture.command("/index thread").await;
    let first = fixture
        .until("first index preview", |_, screen| {
            screen.contains("費用preview | 取得済み")
        })
        .await;
    assert_eq!(first.matches("費用preview | 取得済み").count(), 1);
    assert_eq!(first.matches("資料数: 1").count(), 1, "{first}");
    // This private disposable DB change creates a broken write-space lookup
    // without emitting Changed, so the next explicit preview fails in Core.
    fixture.control.store.database.call(|db| {
        db.execute("INSERT OR REPLACE INTO app_meta(key,value) VALUES('index.active_space','missing-fixture-space')", [])?;
        Ok(())
    }).await.unwrap();
    let actions = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('p'),
        KeyModifiers::NONE,
    )));
    assert_eq!(actions.len(), 1, "one explicit preview request");
    for action in actions {
        fixture.actions.send(action).await.unwrap();
    }
    let failed = fixture
        .until("failed index preview", |_, screen| {
            screen.contains("費用preview | 取得失敗")
        })
        .await;
    assert_eq!(failed.matches("費用preview | 取得失敗").count(), 1);
    assert_eq!(
        failed.matches("区間数: 1").count(),
        1,
        "previous result must remain visible: {failed}"
    );
    assert_eq!(failed.matches("前回の結果 | 古い情報").count(), 1);
    assert_eq!(fixture.index_preview_ready, 1);
    assert_eq!(fixture.index_preview_failed, 1);
    let actions = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('b'),
        KeyModifiers::NONE,
    )));
    assert!(
        actions.is_empty(),
        "failed preview must not allow confirmation"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unrelated_owner_changed_keeps_index_confirmation_and_preview_usable() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut fixture = Fixture::build_in(
        "http://127.0.0.1:9/v1",
        false,
        false,
        shellboy::config::UiLanguage::Ja,
    )
    .await;
    let cwd = fixture.root.path().to_str().unwrap().to_owned();
    fixture.control.store.database.call(move |db| {
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('unrelated','before',?1,'','fixture','fixture0','now')", [cwd])?;
        Ok(())
    }).await.unwrap();
    fixture.command("/index thread").await;
    fixture
        .until("index captured", |_, screen| {
            screen.contains("費用preview | 取得済み")
        })
        .await;
    assert_eq!(fixture.index_preview_ready, 1);
    assert_eq!(fixture.index_preview_failed, 0);
    let actions = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('b'),
        KeyModifiers::NONE,
    )));
    assert!(actions.is_empty());
    let before = fixture.pump().await;
    assert_eq!(before.matches("索引処理の実行確認").count(), 1, "{before}");
    assert!(before.contains("資料数: 1"), "{before}");
    let (result, _) = fixture
        .cli_thread(
            "unrelated",
            &["thread", "set", "--thread", "unrelated", "--title", "after"],
        )
        .await;
    assert_eq!(result["ok"], true, "{result}");
    let after = fixture.pump().await;
    assert!(!after.contains("再確認が必要"), "{after}");
    assert!(after.contains("資料数: 1"), "{after}");
    let actions = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::NONE,
    )));
    assert!(
        matches!(actions.as_slice(), [UiAction::ConfirmedIndex { .. }]),
        "another thread's change must not block a reviewed confirmation: {actions:?}"
    );
    for action in actions {
        fixture.actions.send(action).await.unwrap();
    }
    let reported = fixture.pump().await;
    assert!(!reported.contains("古くなりました"), "{reported}");
    assert_eq!(fixture.index_preview_ready, 1, "no re-preview was needed");
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unrelated_owner_changed_does_not_block_a_reviewed_deletion() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut fixture = Fixture::build_in(
        "http://127.0.0.1:9/v1",
        false,
        false,
        shellboy::config::UiLanguage::Ja,
    )
    .await;
    for action in fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('t'),
        KeyModifiers::CONTROL,
    ))) {
        fixture.actions.send(action).await.unwrap();
    }
    fixture
        .until("selected browser row", |ui, _| {
            ui.threads_visible() && ui.threads().iter().any(|row| row.thread_id == "reactive")
        })
        .await;
    let actions = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL,
    )));
    assert!(matches!(
        actions.as_slice(),
        [UiAction::ThreadDeletion { execute: false, .. }]
    ));
    for action in actions {
        fixture.actions.send(action).await.unwrap();
    }
    let before = fixture
        .observed_until("deletion preview response", |f, _| {
            f.deletion_preview_responses == 1
        })
        .await;
    assert!(
        before.contains("削除できます。Y で実行します。"),
        "{before}"
    );
    assert_eq!(before.matches("対象会話: 1").count(), 1, "{before}");
    assert_eq!(fixture.deletion_preview_responses, 1);

    let cwd = fixture.root.path().to_str().unwrap().to_owned();
    fixture.control.store.database.call(move |db| {
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('unrelated-delete','before',?1,'','fixture','fixture0','now')", [cwd])?;
        Ok(())
    }).await.unwrap();
    let (result, _) = fixture
        .cli_thread(
            "unrelated-delete",
            &[
                "thread",
                "set",
                "--thread",
                "unrelated-delete",
                "--title",
                "after",
            ],
        )
        .await;
    assert_eq!(result["ok"], true, "{result}");
    let after = fixture.pump().await;
    assert!(!after.contains("確認が古くなりました"), "{after}");
    assert_eq!(after.matches("対象会話: 1").count(), 1, "{after}");
    let actions = fixture.ui.event(Event::Key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::NONE,
    )));
    assert!(
        matches!(
            actions.as_slice(),
            [UiAction::ThreadDeletion { execute: true, .. }]
        ),
        "another thread's change must not block Y: {actions:?}"
    );
    for action in actions {
        fixture.actions.send(action).await.unwrap();
    }
    assert_eq!(
        fixture.deletion_preview_responses, 1,
        "no re-preview was needed"
    );
    let deleted = watchdog("reviewed deletion applied", async {
        loop {
            fixture.pump().await;
            let live = fixture
                .control
                .store
                .database
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT deleted_at IS NULL FROM thread WHERE id='reactive'",
                        [],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if !live {
                return true;
            }
            fixture.wait_update_or_action().await;
        }
    })
    .await;
    assert!(deleted.unwrap());
    fixture.finish().await;
}

/// #141: the status bar must show saved provider usage for a thread past the
/// bounded display window (more than 33 turns), through the real owner path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_bar_shows_saved_context_usage_for_a_long_thread_through_the_real_owner() {
    let mut fixture = Fixture::build("http://127.0.0.1:9/v1", true, true).await;
    // Header/history can precede CurrentView. Completed activity stops polling,
    // so force that order and use a real Changed event, not a lucky startup tick.
    let initial = fixture
        .until("initial completed activity", |ui, _| {
            ui.view().state == shellboy::activity::ActivityPhase::Completed.label()
        })
        .await;
    assert!(
        initial.contains("^-"),
        "no context is adopted before CurrentView: {initial}"
    );
    assert!(
        fixture.current_view_trace.is_empty(),
        "initial snapshots are held"
    );
    for update in fixture.held_initial_views.take().unwrap() {
        fixture.accept_update(update);
    }
    fixture
        .cli(&[
            "thread",
            "set",
            "--thread",
            "reactive",
            "--title",
            "CONTEXT_USAGE_READY",
        ])
        .await;
    let screen = fixture
        .observed_until(
            "saved context usage on the status bar",
            |fixture, screen| {
                fixture.ui.view().title == "CONTEXT_USAGE_READY"
                    && saved_context_status_ready(fixture.current_view_trace.back(), screen)
            },
        )
        .await;
    assert!(!screen.contains("^-"), "{screen}");
    assert!(screen.contains("Cache 65%"), "{screen}");
    fixture.finish().await;
}

fn saved_context_status_ready(snapshot: Option<&Value>, screen: &str) -> bool {
    snapshot.is_some_and(|snapshot| {
        snapshot["thread"] == "reactive"
            && snapshot["title"] == "CONTEXT_USAGE_READY"
            && snapshot["input_tokens"] == 159534
            && snapshot["cached_tokens"] == 103424
    }) && screen.contains("^159K")
        && screen.contains("Cache 65%")
}

#[test]
fn saved_context_wait_requires_matching_snapshot_and_footer() {
    let screen = "^159K/200K v- Cache 65%";
    let snapshot = json!({"thread":"reactive","title":"CONTEXT_USAGE_READY","input_tokens":159534,"cached_tokens":103424});
    assert!(!saved_context_status_ready(None, screen));
    assert!(!saved_context_status_ready(Some(&snapshot), "^-"));
    let mut old = snapshot.clone();
    old["title"] = json!("BASE_TITLE");
    assert!(!saved_context_status_ready(Some(&old), screen));
    old = snapshot.clone();
    old["input_tokens"] = Value::Null;
    assert!(!saved_context_status_ready(Some(&old), screen));
    assert!(saved_context_status_ready(Some(&snapshot), screen));
}
