//! Automatic Responses retries through a real owner, using only synthetic loopback traffic.
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    control::{Control, EngineEvent},
    protocol::*,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

// Fixture progress/cleanup bounds are not product latency assertions.

struct Incoming {
    body: Value,
    reply: oneshot::Sender<Reply>,
}

struct Reply {
    bytes: Vec<u8>,
    hold_open: bool,
}

impl Reply {
    fn http(status: u16, content_type: &str, body: &str, extra_headers: &str) -> Self {
        Self {
            bytes: format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
            hold_open: false,
        }
    }

    fn error(status: u16, extra_headers: &str) -> Self {
        Self::http(
            status,
            "application/json",
            r#"{"error":{"code":"synthetic_failure","message":"fixture"}}"#,
            extra_headers,
        )
    }

    fn success() -> Self {
        Self::http(
            200,
            "application/json",
            &json!({"id":"response_retry_fixture","status":"completed","output":[{
                "type":"message","role":"assistant","status":"completed",
                "content":[{"type":"output_text","text":"recovered"}]
            }]})
            .to_string(),
            "",
        )
    }

    fn partial(body: &str, hold_open: bool) -> Self {
        Self {
            // Deliberately advertise an incomplete body to distinguish transport loss
            // from a clean but invalid Responses document.
            bytes: format!(
                "HTTP/1.1 200 Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: 99999\r\nConnection: close\r\n\r\n{body}"
            )
            .into_bytes(),
            hold_open,
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "client closed before the request body");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() < 1024 * 1024, "unexpected fixture request size");
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..index]);
            if head.starts_with("GET /v1/models ") {
                return Value::Null;
            }
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap();
            if bytes.len() >= index + 4 + length {
                return serde_json::from_slice(&bytes[index + 4..index + 4 + length]).unwrap();
            }
        }
    }
}

async fn proxy(log: std::path::PathBuf) -> (String, mpsc::Receiver<Incoming>, JoinHandle<()>) {
    std::fs::write(&log, "").unwrap();
    assert!(
        std::fs::read(&log).unwrap().is_empty(),
        "request JSONL starts at zero rows"
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    stream.set_nodelay(true).unwrap();
                    let sender = sender.clone();
                    let log = log.clone();
                    connections.spawn(async move {
                        let body = read_request(&mut stream).await;
                        use std::io::Write;
                        writeln!(std::fs::OpenOptions::new().append(true).open(log).unwrap(), "{}", body).unwrap();
                        let (reply, receive) = oneshot::channel();
                        if sender.send(Incoming { body, reply }).await.is_err() {
                            return;
                        }
                        let Ok(reply) = receive.await else { return; };
                        if stream.write_all(&reply.bytes).await.is_ok() && reply.hold_open {
                            let mut byte = [0];
                            let _ = stream.read(&mut byte).await;
                        }
                    });
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    completed.unwrap().unwrap();
                }
            }
        }
    });
    (url, receiver, task)
}

struct Fixture {
    _root: tempfile::TempDir,
    owner: DataOwner,
    control: Arc<Control>,
    requests: mpsc::Receiver<Incoming>,
    server: JoinHandle<()>,
    accepted: MessageAccepted,
}

#[derive(Default)]
struct FixtureSettings {
    auto_retry: Option<bool>,
    connection: &'static str,
    max_tokens: Option<u64>,
    max_model_requests: Option<std::num::NonZeroU32>,
}

impl Fixture {
    async fn new(retries: u32, max_delay_ms: u64, idle_ms: u64) -> Self {
        Self::with_history(
            retries,
            max_delay_ms,
            idle_ms,
            true,
            None,
            "synthetic retry regression",
        )
        .await
    }

    async fn with_history(
        retries: u32,
        max_delay_ms: u64,
        idle_ms: u64,
        trim_enabled: bool,
        seed_chars: Option<usize>,
        text: &str,
    ) -> Self {
        Self::configured(
            retries,
            max_delay_ms,
            idle_ms,
            trim_enabled,
            seed_chars,
            text,
            FixtureSettings::default(),
        )
        .await
    }

    async fn configured(
        retries: u32,
        max_delay_ms: u64,
        idle_ms: u64,
        trim_enabled: bool,
        seed_chars: Option<usize>,
        text: &str,
        settings: FixtureSettings,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (url, requests, server) = proxy(root.path().join("requests.jsonl")).await;
        let config = root.path().join("config.toml");
        let trim_settings = format!("auto_trim_on_disconnect={trim_enabled}\n");
        let recovery_settings = settings.auto_retry.map_or_else(String::new, |enabled| {
            format!("[recovery]\nauto_retry_usage_limit={enabled}\n")
        });
        std::fs::write(
            &config,
            format!(
                "default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms={max_delay_ms}\nstream_idle_timeout_ms={idle_ms}\nresponse_header_timeout_ms={idle_ms}\n{trim_settings}{}{recovery_settings}", settings.connection
            ),
        )
        .unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let thread = request(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_thread_id: None,
                connection: None,
                model: None,
                effort: None,
            }),
        )
        .await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        if let Some(max_tokens) = settings.max_tokens {
            request(
                &control,
                Command::ThreadSet(Box::new(ThreadSetArgs {
                    thread_id: Some(thread.clone()),
                    max_tokens: Some(max_tokens.to_string()),
                    ..Default::default()
                })),
            )
            .await;
        }
        if let Some(seed_chars) = seed_chars {
            let target = thread.clone();
            control.store.database.call(move |db| {
                let tx = db.transaction()?;
                shellboy::history::append_node(&tx, &target, "old-large-context", None, "user_message",
                    &json!({"role":"user","content":[{"type":"input_text","text":format!("DENSE_OLD_CONTEXT {}", "x".repeat(seed_chars))}]}), "2026-10-01T00:00:00Z")?;
                shellboy::history::append_node(&tx, &target, "old-answer", None, "assistant_response",
                    &json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old completed response"}]}]}), "2026-10-01T00:00:01Z")?;
                tx.commit()?;
                Ok(())
            }).await.unwrap();
        }
        let accepted = serde_json::from_value(
            request(
                &control,
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: settings.max_model_requests,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: Deliver::Next,
                    thread_id: Some(thread),
                    text: text.into(),
                }),
            )
            .await,
        )
        .unwrap();
        Self {
            _root: root,
            owner,
            control,
            requests,
            server,
            accepted,
        }
    }

    #[track_caller]
    fn incoming(&mut self) -> impl std::future::Future<Output = Incoming> + '_ {
        self.incoming_observed()
    }

    #[track_caller]
    fn incoming_after_tool(&mut self) -> impl std::future::Future<Output = Incoming> + '_ {
        self.incoming_observed()
    }

    #[track_caller]
    fn incoming_observed(&mut self) -> impl std::future::Future<Output = Incoming> + '_ {
        let caller = std::panic::Location::caller();
        async move {
            match crate::test_watchdog::watchdog(
                "tests/response_retries.rs: incoming_with_timeout",
                self.requests.recv(),
            )
            .await
            {
                Ok(Some(value)) => value,
                result => {
                    let diagnostic = crate::test_watchdog::watchdog(
                        "tests/response_retries.rs: incoming_with_timeout",
                        progress(&self.control, self.accepted.turn_id.clone()),
                    )
                    .await;
                    panic!(
                        "HTTP request watchdog at {}:{}; receive={}; saved lifecycle facts={diagnostic:?}",
                        caller.file(),
                        caller.line(),
                        if result.is_err() {
                            "timeout"
                        } else {
                            "sender closed"
                        }
                    );
                }
            }
        }
    }

    async fn attempts(&self) -> Vec<(String, Option<Value>)> {
        let turn = self.accepted.turn_id.clone().unwrap();
        self.control.store.database.call(move |db| {
            let rows = db.prepare("SELECT state,CASE WHEN error_json IS NULL THEN NULL ELSE json_set(error_json,'$.rate_observation',json_extract(input_manifest_json,'$.rate_observation')) END FROM model_attempt WHERE turn_id=?1 ORDER BY rowid")?
                .query_map([turn], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows.into_iter().map(|(state, error)| (state, error.map(|value| serde_json::from_str(&value).unwrap()))).collect())
        }).await.unwrap()
    }

    async fn reduction_outcomes(&self) -> Vec<Option<String>> {
        let turn = self.accepted.turn_id.clone().unwrap();
        self.control.store.database.call(move |db| {
            db.prepare("SELECT c.outcome FROM model_attempt a LEFT JOIN automatic_compaction c ON c.attempt_id=a.id WHERE a.turn_id=?1 ORDER BY a.rowid")?
                .query_map([turn], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>().map_err(Into::into)
        }).await.unwrap()
    }

    #[track_caller]
    fn settled(&self) -> impl std::future::Future<Output = String> + '_ {
        let caller = std::panic::Location::caller();
        let turn = self.accepted.turn_id.clone().unwrap();
        let thread = self.accepted.thread_id.clone();
        let mut events = self.control.subscribe();
        async move {
            let result =
                crate::test_watchdog::watchdog("tests/response_retries.rs: settled", async {
                    loop {
                        let turn = turn.clone();
                        let state: String = self
                            .control
                            .store
                            .database
                            .call(move |db| {
                                Ok(db.query_row(
                                    "SELECT state FROM turn WHERE id=?1",
                                    [turn],
                                    |row| row.get(0),
                                )?)
                            })
                            .await
                            .unwrap();
                        if !matches!(state.as_str(), "preparing" | "running" | "waiting") {
                            return state;
                        }
                        changed(&mut events, &thread, caller).await;
                    }
                })
                .await;
            match result {
                Ok(state) => state,
                Err(_) => {
                    let diagnostic = crate::test_watchdog::watchdog(
                        "tests/response_retries.rs: settled",
                        progress(&self.control, Some(turn)),
                    )
                    .await;
                    panic!(
                        "Turn did not settle within fixture watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                        caller.file(),
                        caller.line()
                    );
                }
            }
        }
    }

    #[track_caller]
    fn waiting_for_retry(&self) -> impl std::future::Future<Output = ()> + '_ {
        let caller = std::panic::Location::caller();
        let thread = self.accepted.thread_id.clone();
        let mut events = self.control.subscribe();
        async move {
            let result = crate::test_watchdog::watchdog(
                "tests/response_retries.rs: waiting_for_retry",
                async {
                    loop {
                        if self.attempts().await.iter().any(|(state, error)| {
                            state == "failed"
                                && error
                                    .as_ref()
                                    .is_some_and(|value| value.get("automatic_retry").is_some())
                        }) {
                            return;
                        }
                        // The executor publishes Changed after the automatic_retry
                        // transaction commits, before releasing the boundary for backoff.
                        changed(&mut events, &thread, caller).await;
                    }
                },
            )
            .await;
            if result.is_err() {
                let diagnostic = crate::test_watchdog::watchdog(
                    "tests/response_retries.rs: waiting_for_retry",
                    progress(&self.control, self.accepted.turn_id.clone()),
                )
                .await;
                panic!(
                    "Automatic retry was not persisted within fixture watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                    caller.file(),
                    caller.line()
                );
            }
        }
    }

    async fn no_more_requests(&mut self) {
        assert!(self.requests.try_recv().is_err(), "unexpected HTTP replay");
    }

    #[track_caller]
    fn close(self) -> impl std::future::Future<Output = ()> {
        let caller = std::panic::Location::caller();
        async move {
            match crate::test_watchdog::watchdog(
                "tests/response_retries.rs: close",
                self.control.shutdown(),
            )
            .await
            {
                Ok(result) => result.unwrap(),
                Err(_) => {
                    let diagnostic = crate::test_watchdog::watchdog(
                        "tests/response_retries.rs: close",
                        progress(&self.control, self.accepted.turn_id.clone()),
                    )
                    .await;
                    panic!(
                        "Control shutdown watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                        caller.file(),
                        caller.line()
                    );
                }
            }
            match crate::test_watchdog::watchdog(
                "tests/response_retries.rs: close",
                self.owner.shutdown(),
            )
            .await
            {
                Ok(result) => result.unwrap(),
                Err(_) => {
                    let diagnostic = crate::test_watchdog::watchdog(
                        "tests/response_retries.rs: close",
                        progress(&self.control, self.accepted.turn_id.clone()),
                    )
                    .await;
                    panic!(
                        "Owner shutdown watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                        caller.file(),
                        caller.line()
                    );
                }
            }
            self.server.abort();
            let _ = self.server.await;
        }
    }
}

// Watchdogs are fixture deadlock guards. Failure reports retain bounded saved
// states/phase codes only, never request bodies, raw errors or response text.
async fn progress(
    control: &Control,
    turn: Option<String>,
) -> Result<Value, shellboy::storage::StorageError> {
    control.store.database.call(move |db| {
        let mut state = serde_json::Map::new();
        for (name, query) in [
            ("runs", "SELECT json_object('id',substr(id,1,128),'state',substr(state,1,64),'stop_reason',substr(stop_reason,1,128)) FROM run WHERE (?1 IS NULL OR id=(SELECT run_id FROM turn WHERE id=?1)) ORDER BY rowid DESC LIMIT 8"),
            ("turns", "SELECT json_object('id',substr(id,1,128),'state',substr(state,1,64),'generation',generation,'instruction_snapshot_present',instruction_snapshot_id IS NOT NULL,'recovery_code',substr(json_extract(recovery_reason_json,'$.code'),1,128)) FROM turn WHERE (?1 IS NULL OR id=?1) ORDER BY rowid DESC LIMIT 8"),
            ("attempts", "SELECT json_object('id',substr(id,1,128),'state',substr(state,1,64)) FROM model_attempt WHERE (?1 IS NULL OR turn_id=?1) ORDER BY rowid DESC LIMIT 8"),
        ] {
            let rows = db.prepare(query)?.query_map([turn.as_deref()], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            state.insert(name.into(), json!(rows));
        }
        Ok(Value::Object(state))
    }).await
}
async fn changed(
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    thread: &str,
    caller: &'static std::panic::Location<'static>,
) {
    loop {
        match events.recv().await {
            Ok(EngineEvent::Changed { thread_id, .. }) if thread_id == thread => return,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!(
                "Fixture event stream closed at {}:{} for Thread {thread}",
                caller.file(),
                caller.line()
            ),
            _ => {}
        }
    }
}
#[track_caller]
fn request(
    control: &Arc<Control>,
    command: Command,
) -> impl std::future::Future<Output = Value> + '_ {
    let caller = std::panic::Location::caller();
    async move {
        let response = match crate::test_watchdog::watchdog(
            "tests/response_retries.rs: request",
            control.request(Request::new(
                format!(
                    "{}:{}",
                    control.store.operation_epoch.current(),
                    uuid::Uuid::new_v4()
                ),
                command,
            )),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => {
                let diagnostic = crate::test_watchdog::watchdog(
                    "tests/response_retries.rs: request",
                    progress(control, None),
                )
                .await;
                panic!(
                    "Control request watchdog at {}:{}; saved lifecycle facts={diagnostic:?}",
                    caller.file(),
                    caller.line()
                );
            }
        };
        assert!(response.ok, "{:?}", response.error);
        response.result.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_http_errors_retry_consumed_error_bodies_with_bounded_backoff() {
    for status in [429, 500, 502, 503, 504] {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        let first = fixture.incoming().await;
        let original = first.body;
        let retry_after = "Retry-After: invalid-fixture\r\n";
        first
            .reply
            .send(Reply::error(status, retry_after))
            .ok()
            .unwrap();
        let second = fixture.incoming().await;
        let saved_before_retry = fixture.attempts().await[0].clone();
        assert_eq!(second.body, original, "retry changed the prepared request");
        second.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(fixture.settled().await, "completed", "HTTP {status}");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].0, "failed");
        assert_eq!(attempts[1].0, "completed");
        let first_error = attempts[0].1.as_ref().unwrap();
        assert_eq!(first_error["rate_observation"]["status"], status);
        assert_eq!(first_error["rate_observation"]["retry_after_present"], true);
        assert!(first_error["rate_observation"]["retry_after_ms"].is_null());
        assert_eq!(first_error["rate_observation"]["applied_wait_ms"], 0);
        assert_eq!(attempts[0], saved_before_retry);
        let turn = fixture.accepted.turn_id.clone().unwrap();
        let wait=fixture.control.store.database.call({let turn=turn.clone(); move |db| Ok(db.query_row("SELECT json_extract(input_manifest_json,'$.rate_observation.applied_wait_ms') FROM model_attempt WHERE turn_id=?1 AND state='completed'",[turn],|r|r.get::<_,i64>(0))?)}).await.unwrap();
        assert!(wait >= 10);
        for group_by in [None, Some("thread".into())] {
            let grouped = group_by.is_some();
            let result = request(
                &fixture.control,
                Command::UsageGet(UsageGetArgs {
                    turn_id: Some(turn.clone()),
                    group_by,
                    ..Default::default()
                }),
            )
            .await;
            let result = if grouped {
                &result["items"][0]
            } else {
                &result
            };
            let rates = &result["connection_rates"]["by_connection"]["fixture"];
            assert_eq!(rates["applied_wait_ms"], wait);
            assert_eq!(rates["http_429"], u64::from(status == 429));
            assert_eq!(rates["http_5xx"], u64::from(status >= 500));
        }
        let retry = &first_error["automatic_retry"];
        assert_eq!(retry["number"], 1);
        assert_eq!(retry["limit"], 2);
        assert_eq!(retry["delay_ms"], 10);
        assert_eq!(retry["possible_duplicate_charge"], true);
        let recovery_artifacts: i64 = fixture
            .control
            .store
            .database
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM node WHERE kind='incomplete_response'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(recovery_artifacts, 0);
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_budget_is_exact_and_zero_disables_retries() {
    for (limit, status) in [(0, 503), (2, 503), (0, 429), (2, 429)] {
        let mut fixture = Fixture::new(limit, 10, 5_000).await;
        for _ in 0..=limit {
            fixture
                .incoming()
                .await
                .reply
                .send(Reply::error(status, ""))
                .ok()
                .unwrap();
        }
        assert_eq!(fixture.settled().await, "recovery_wait");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), limit as usize + 1);
        for (index, (state, error)) in attempts.iter().enumerate().take(limit as usize) {
            assert_eq!(state, "failed");
            assert_eq!(
                error.as_ref().unwrap()["automatic_retry"]["number"],
                index + 1
            );
        }
        let (state, error) = attempts.last().unwrap();
        assert_eq!(state, "incomplete");
        let error = error.as_ref().unwrap();
        assert!(error.get("automatic_retry").is_none());
        assert_eq!(error["rate_observation"]["status"], status);
        assert_eq!(error["rate_observation"]["retry_after_present"], false);
        let wait = error["rate_observation"]["applied_wait_ms"]
            .as_u64()
            .unwrap();
        assert_eq!(wait == 0, limit == 0);
        if limit > 0 {
            assert_eq!(error["automatic_retry_exhausted"]["retries"], limit);
            assert_eq!(error["automatic_retry_exhausted"]["limit"], limit);
        }
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_tool_groups_reset_consecutive_budget_but_context_overflow_does_not() {
    let mut fixture = Fixture::new(2, 10, 5_000).await;
    let mut pending = fixture.incoming().await;
    for number in 1..=3 {
        pending.reply.send(Reply::error(503, "")).ok().unwrap();
        let retried = fixture.incoming().await;
        let response = json!({
            "id":format!("response-tool-{number}"),"status":"completed",
            "output":[{"type":"function_call","name":"shell",
                "call_id":format!("call-{number}"),
                "arguments":json!({"command":format!("echo retry-group-{number}")}).to_string()}]
        });
        retried
            .reply
            .send(Reply::http(
                200,
                "application/json",
                &response.to_string(),
                "",
            ))
            .ok()
            .unwrap();
        pending = fixture.incoming_after_tool().await;
        assert!(
            pending.body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| {
                    item["type"] == "function_call_output"
                        && item["call_id"] == format!("call-{number}")
                })
        );
    }
    pending.reply.send(Reply::error(503, "")).ok().unwrap();
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(503, ""))
        .ok()
        .unwrap();
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(503, ""))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 9);
    assert_eq!(
        attempts.last().unwrap().1.as_ref().unwrap()["automatic_retry_exhausted"],
        json!({"retries":2,"limit":2})
    );
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn losing_an_empty_response_body_retries_before_any_successful_response_bytes() {
    for reply in [
        Reply {
            bytes: Vec::new(),
            hold_open: false,
        },
        Reply::partial("", false),
        Reply::http(200, "text/event-stream", "", ""),
    ] {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        fixture.incoming().await.reply.send(reply).ok().unwrap();
        fixture
            .incoming()
            .await
            .reply
            .send(Reply::success())
            .ok()
            .unwrap();
        assert_eq!(fixture.settled().await, "completed");
        assert_eq!(fixture.attempts().await.len(), 2);
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metadata_and_partial_output_transport_loss_retry_by_default_without_executing_tools() {
    let bodies = [
        ":".to_owned(),
        ": heartbeat\n\n".to_owned(),
        format!(
            "data: {}\n\n",
            json!({"type":"response.reasoning_summary_text.delta","delta":"thinking"})
        ),
        format!(
            "data: {}\n\n",
            json!({"type":"response.function_call_arguments.delta","item_id":"synthetic_call","delta":"{\"command\":"})
        ),
        format!(
            "data: {}\n\n",
            json!({"type":"response.output_text.delta","delta":"visible partial text"})
        ),
    ];
    let replies = bodies
        .iter()
        .map(|body| Reply::partial(body, false))
        .chain([Reply::http(200, "text/event-stream", ":", "")]);
    for reply in replies {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        fixture.incoming().await.reply.send(reply).ok().unwrap();
        let retried = fixture.incoming().await;
        assert!(!retried.body.to_string().contains("visible partial text"));
        assert!(!retried.body.to_string().contains("synthetic_call"));
        retried.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(fixture.settled().await, "completed");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 2);
        assert!(
            attempts[0]
                .1
                .as_ref()
                .unwrap()
                .get("automatic_retry")
                .is_some()
        );
        let tools: i64 = fixture
            .control
            .store
            .database
            .call(|db| {
                Ok(db.query_row("SELECT count(*) FROM shell_execution", [], |row| row.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(tools, 0, "incomplete streamed tool arguments executed");
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permanent_http_and_successful_protocol_failures_are_not_retried() {
    for reply in [
        Reply::error(400, ""),
        Reply::error(401, ""),
        Reply::error(403, ""),
        Reply::error(422, ""),
        Reply::http(200, "application/json", "{invalid", ""),
    ] {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        fixture.incoming().await.reply.send(reply).ok().unwrap();
        assert_eq!(fixture.settled().await, "recovery_wait");
        assert_eq!(fixture.attempts().await.len(), 1);
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_empty_or_partial_response_retries() {
    for body in [
        "",
        ": heartbeat\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"timeout-partial\"}\n\n",
    ] {
        let mut fixture = Fixture::new(2, 10, 300).await;
        fixture
            .incoming()
            .await
            .reply
            .send(Reply::partial(body, true))
            .ok()
            .unwrap();
        fixture
            .incoming()
            .await
            .reply
            .send(Reply::success())
            .ok()
            .unwrap();
        assert_eq!(fixture.settled().await, "completed");
        assert_eq!(fixture.attempts().await.len(), 2);
        let attempts = fixture.attempts().await;
        assert!(
            attempts[0]
                .1
                .as_ref()
                .unwrap()
                .to_string()
                .contains("timeout")
        );
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_error_body_preserves_retryable_status_and_can_recover() {
    for status in [429, 500, 502, 503, 504] {
        let mut fixture = Fixture::new(2, 10, 500).await;
        fixture.incoming().await.reply.send(Reply {
            bytes: format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: 99999\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{{").into_bytes(),
            hold_open: true,
        }).ok().unwrap();
        fixture
            .incoming()
            .await
            .reply
            .send(Reply::success())
            .ok()
            .unwrap();
        assert_eq!(fixture.settled().await, "completed", "status {status}");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 2, "status {status}");
        assert_eq!(attempts[0].0, "failed");
        let saved = attempts[0].1.as_ref().unwrap();
        assert_eq!(saved["error"]["kind"], "timeout");
        assert_eq!(saved["error"]["status"], status);
        assert!(
            saved["error"]["message"]
                .as_str()
                .unwrap()
                .contains("connections.fixture.stream_idle_timeout_ms")
        );
        assert_eq!(saved["automatic_retry"]["number"], 1);
        assert_eq!(attempts[1].0, "completed");
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn stalled_permanent_error_body_is_not_retried() {
    for status in [400, 401, 403, 422] {
        tokio::time::pause();
        // A pending blocking task prevents Tokio's paused clock from auto-
        // advancing while the real loopback headers are still in transit.
        let (release, held) = std::sync::mpsc::channel::<()>();
        let clock = tokio::task::spawn_blocking(move || {
            crate::test_watchdog::blocking_recv("hold virtual loopback clock", &held)
        });
        let mut fixture = Fixture::new(2, 10, 500).await;
        let mut events = fixture.control.subscribe();
        fixture.incoming().await.reply.send(Reply {
            bytes: format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: 99999\r\nConnection: close\r\n\r\n").into_bytes(),
            hold_open:true,
        }).ok().unwrap();
        crate::test_watchdog::watchdog("permanent response headers observed", async {
            // Receive-only progress is sampled, not a text-wait completion.
            // Virtual time is deliberately frozen until headers are observed.
            loop {
                let page: EventsWaitResult = serde_json::from_value(
                    request(
                        &fixture.control,
                        Command::EventsWait(EventsWaitArgs {
                            after: None,
                            thread_ids: vec![fixture.accepted.thread_id.clone()],
                            timeout_ms: 0,
                            include_text: true,
                            include_reasoning_activity: false,
                            include_output: false,
                        }),
                    )
                    .await,
                )
                .unwrap();
                if page
                    .live_text
                    .into_iter()
                    .flatten()
                    .any(|attempt| attempt.receive_age_ms.is_some())
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        crate::test_watchdog::watchdog("saved permanent-body recovery", async {
            loop {
                let turn = fixture.accepted.turn_id.clone().unwrap();
                let state: String = fixture
                    .control
                    .store
                    .database
                    .call(move |db| {
                        Ok(
                            db.query_row("SELECT state FROM turn WHERE id=?1", [turn], |row| {
                                row.get(0)
                            })?,
                        )
                    })
                    .await
                    .unwrap();
                if state == "recovery_wait" {
                    break;
                }
                events.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        tokio::time::resume();
        release.send(()).unwrap();
        clock.await.unwrap().unwrap();
        assert_eq!(fixture.settled().await, "recovery_wait", "status {status}");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 1, "status {status}: {attempts:?}");
        assert_eq!(
            fixture_request_log_lines(&fixture),
            1,
            "one actual HTTP request"
        );
        let saved = attempts[0].1.as_ref().unwrap();
        assert_eq!(saved["error"]["kind"], "timeout");
        assert_eq!(saved["error"]["status"], status);
        assert!(saved.get("automatic_retry").is_none());
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_cancels_a_persisted_retry_wait_without_another_request() {
    let mut fixture = Fixture::new(2, 30_000, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 30\r\n"))
        .ok()
        .unwrap();
    fixture.waiting_for_retry().await;
    crate::test_watchdog::watchdog(
        "tests/response_retries.rs: stop_cancels_a_persisted_retry_wait_without_another_request",
        request(
            &fixture.control,
            Command::RunStop(shellboy::protocol::RunStopTarget::Run {
                run_id: fixture.accepted.run_id.clone().unwrap(),
            }),
        ),
    )
    .await
    .expect("stop did not finish within fixture watchdog");
    assert_eq!(fixture.settled().await, "stopped");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 1);
    let rate = &attempts[0].1.as_ref().unwrap()["rate_observation"];
    assert_eq!(rate["status"], 429);
    assert_eq!(rate["retry_after_ms"], 30_000);
    assert_eq!(rate["applied_wait_ms"], 0);
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_shutdown_cancels_a_persisted_retry_wait_without_another_request() {
    let mut fixture = Fixture::new(2, 30_000, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(503, "Retry-After: 30\r\n"))
        .ok()
        .unwrap();
    fixture.waiting_for_retry().await;
    crate::test_watchdog::watchdog("tests/response_retries.rs: owner_shutdown_cancels_a_persisted_retry_wait_without_another_request", fixture.control.shutdown())
        .await
        .expect("shutdown did not finish within fixture watchdog")
        .unwrap();
    assert_eq!(fixture.attempts().await.len(), 1);
    fixture.no_more_requests().await;
    crate::test_watchdog::watchdog("tests/response_retries.rs: owner_shutdown_cancels_a_persisted_retry_wait_without_another_request", fixture.owner.shutdown())
        .await
        .expect("owner shutdown did not finish")
        .unwrap();
    fixture.server.abort();
    let _ = fixture.server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_retry_after_is_honored_independently_of_short_exponential_cap() {
    let mut fixture = Fixture::new(2, 10, 5_000).await;
    let first = fixture.incoming().await;
    first
        .reply
        .send(Reply::error(429, "Retry-After: 1\r\n"))
        .ok()
        .unwrap();
    let second = fixture.incoming().await;
    second.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(fixture.settled().await, "completed");
    assert_eq!(
        fixture.attempts().await[0].1.as_ref().unwrap()["automatic_retry"]["delay_ms"],
        1000
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_retry_after_over_ten_minutes_is_deferred_instead_of_shortened() {
    let mut fixture = Fixture::new(2, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(503, "Retry-After: 601\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 1);
    let error = attempts[0].1.as_ref().unwrap();
    assert!(error.get("automatic_retry").is_none());
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("automatic wait limit is 600s")
    );
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_retry_is_audited_outside_selected_history_and_never_replayed_or_executed() {
    let mut fixture = Fixture::new(2, 10, 5_000).await;
    let tool = json!({"type":"function_call","id":"unadopted_call","call_id":"must_not_execute",
        "name":"shell","arguments":"{\"command\":\"echo never-run\"}"});
    let body = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_text.delta","delta":"paid-partial-fixture"}),
        json!({"type":"response.output_item.done","output_index":0,"item":tool})
    );
    fixture
        .incoming()
        .await
        .reply
        .send(terminal_sse(&body, TERMINAL_SSE_500, ""))
        .ok()
        .unwrap();
    let next = fixture.incoming().await;
    assert!(!next.body.to_string().contains("paid-partial-fixture"));
    assert!(!next.body.to_string().contains("must_not_execute"));
    next.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(fixture.settled().await, "completed");
    let attempts = fixture.attempts().await;
    let failure = attempts[0].1.as_ref().unwrap();
    assert_terminal_stream_failure(failure, Some(500));
    let retry = &failure["automatic_retry"];
    assert_eq!(retry["partial_response_discarded"], true);
    assert_eq!(retry["possible_duplicate_charge"], true);
    fixture.control.store.database.call(|db| {
        let (saved, selected): (String, i64) = db.query_row(
            "SELECT n.payload_json, EXISTS(SELECT 1 FROM thread_path p WHERE p.node_id=n.id) FROM node n WHERE n.kind='incomplete_response'",
            [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        assert!(saved.contains("paid-partial-fixture"));
        assert!(saved.contains("must_not_execute"));
        assert_eq!(selected, 0, "failed partial response entered model replay");
        assert_eq!(db.query_row("SELECT count(*) FROM shell_execution", [], |row| row.get::<_, i64>(0))?, 0);
        Ok(())
    }).await.unwrap();
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_retry_budget_survives_seven_explicit_proxy_failures() {
    let defaults = shellboy::config::Connection::default();
    let mut fixture = Fixture::new(
        defaults.response_max_retries,
        1, // Native wiring uses a small configured cap; policy delays are inspected separately.
        5_000,
    )
    .await;
    for _ in 0..7 {
        fixture
            .incoming()
            .await
            .reply
            .send(Reply::error(503, ""))
            .ok()
            .unwrap();
    }
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::success())
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "completed");
    let attempts = fixture.attempts().await;
    assert!(attempts.len() >= 7);
    assert_eq!(attempts.last().unwrap().0, "completed");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_eligible_failure_reduces_but_first_retry_is_unchanged() {
    for mode in ["500", "501", "503", "disconnect", "header_timeout"] {
        let mut f = Fixture::with_history(8, 10, 500, true, Some(10_000), "current request").await;
        for index in 0..2 {
            let pending = f.incoming().await;
            assert!(
                pending.body.to_string().contains("DENSE_OLD_CONTEXT"),
                "first retry unchanged {mode}/{index}"
            );
            let reply = match mode {
                "disconnect" => Reply {
                    bytes: Vec::new(),
                    hold_open: false,
                },
                "header_timeout" => Reply {
                    bytes: Vec::new(),
                    hold_open: true,
                },
                _ => Reply::error(mode.parse().unwrap(), ""),
            };
            pending.reply.send(reply).ok().unwrap();
        }
        let reduced = f.incoming().await;
        assert!(!reduced.body.to_string().contains("DENSE_OLD_CONTEXT"));
        assert!(reduced.body.to_string().contains("current request"));
        assert_eq!(reduced.body["tools"].as_array().unwrap().len(), 1);
        reduced.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(f.settled().await, "completed");
        let attempts = f.attempts().await;
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            attempts[1].1.as_ref().unwrap()["automatic_retry"]["number"],
            2
        );
        assert_eq!(
            f.reduction_outcomes().await,
            [None, Some("adopted".into()), None]
        );
        f.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reduced_and_unreducible_inputs_share_the_eight_retry_budget() {
    for old in [None, Some(10_000)] {
        let mut f = Fixture::with_history(8, 10, 5000, true, old, "current protected input").await;
        for _ in 0..9 {
            f.incoming()
                .await
                .reply
                .send(Reply::error(503, ""))
                .ok()
                .unwrap();
        }
        assert_eq!(f.settled().await, "recovery_wait");
        let detail = f
            .control
            .request(Request::new(
                format!(
                    "{}:{}",
                    f.control.store.operation_epoch.current(),
                    uuid::Uuid::new_v4()
                ),
                Command::TurnGet(TurnTarget {
                    turn_id: f.accepted.turn_id.clone().unwrap(),
                }),
            ))
            .await;
        assert!(detail.ok);
        let recovery = &detail.result.as_ref().unwrap()["context"]["recovery_reason"]["details"];
        assert_eq!(recovery["automatic_retries"], 8);
        assert!(
            recovery["recovery"]["protected_request"]["bytes"]
                .as_u64()
                .unwrap()
                > 0
        );
        let actions = recovery["recovery"]["actions"].as_array().unwrap();
        assert!(actions.iter().any(|v| v == "/retry"));
        assert!(!actions.iter().any(|v| v == "/detach"));
        assert!(
            chrono::DateTime::parse_from_rfc3339(recovery["last_failure_at"].as_str().unwrap())
                .is_ok()
        );
        let attempts = f.attempts().await;
        assert_eq!(attempts.len(), 9);
        assert_eq!(
            attempts.last().unwrap().1.as_ref().unwrap()["automatic_retry_exhausted"],
            json!({"retries":8,"limit":8})
        );
        for (i, (_, body)) in attempts[..8].iter().enumerate() {
            assert_eq!(body.as_ref().unwrap()["automatic_retry"]["number"], i + 1);
        }
        f.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_overflow_reduces_immediately_within_the_same_retry_budget() {
    let mut f = Fixture::with_history(1, 10, 5000, true, Some(10_000), "current request").await;
    f.incoming()
        .await
        .reply
        .send(Reply::http(
            400,
            "application/json",
            &json!({"error":{"code":"context_length_exceeded","message":"context limit"}})
                .to_string(),
            "",
        ))
        .ok()
        .unwrap();
    let reduced = f.incoming().await;
    assert!(!reduced.body.to_string().contains("DENSE_OLD_CONTEXT"));
    reduced.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(f.settled().await, "completed");
    assert_eq!(f.attempts().await.len(), 2);
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_automatic_reduction_and_rate_limits_preserve_input() {
    for (enabled, status) in [(false, 503), (true, 429)] {
        let mut f = Fixture::with_history(3, 10, 5000, enabled, Some(10_000), "current").await;
        for _ in 0..3 {
            let request = f.incoming().await;
            assert!(request.body.to_string().contains("DENSE_OLD_CONTEXT"));
            request.reply.send(Reply::error(status, "")).ok().unwrap();
        }
        let request = f.incoming().await;
        assert!(request.body.to_string().contains("DENSE_OLD_CONTEXT"));
        request.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(f.settled().await, "completed");
        f.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_maximum_is_api_free_and_recovery_retry_uses_the_new_model() {
    use shellboy::protocol::{ContextEditAction, ContextEditArgs, ThreadSetArgs};
    let mut f = Fixture::with_history(0, 10, 5000, true, Some(10_000), "current").await;
    f.incoming()
        .await
        .reply
        .send(Reply::error(400, ""))
        .ok()
        .unwrap();
    assert_eq!(f.settled().await, "recovery_wait");
    let thread = f.accepted.thread_id.clone();
    let edit = f
        .control
        .request(Request::new(
            format!(
                "{}:{}",
                f.control.store.operation_epoch.current(),
                uuid::Uuid::new_v4()
            ),
            Command::ContextEdit(ContextEditArgs {
                max_chars: None,
                thread_id: Some(thread.clone()),
                action: ContextEditAction::Maximal,
                from_node_id: None,
                to_node_id: None,
            }),
        ))
        .await;
    assert!(edit.ok, "{:?}", edit.error);
    let operation = edit.operation_id.clone();
    let mut events = f.control.subscribe();
    crate::test_watchdog::watchdog("tests/response_retries.rs: manual_maximum_is_api_free_and_recovery_retry_uses_the_new_model", async {
        loop {
            let result = f
                .control
                .request(Request::new(
                    format!("{}:{}", f.control.store.operation_epoch.current(), uuid::Uuid::new_v4()),
                    Command::OperationGet(shellboy::protocol::OperationTarget {
                        operation_id: operation.clone(),
                    }),
                ))
                .await;
            if result
                .result
                .as_ref()
                .is_some_and(|r| r["downstream"]["state"] == "completed")
            {
                break;
            }
            changed(&mut events, &thread, std::panic::Location::caller()).await;
        }
    })
    .await
    .expect("manual maximum compaction must finish within the fixture watchdog");
    assert_eq!(
        f.attempts().await.len(),
        1,
        "manual edit issues no model request"
    );
    let changed = f
        .control
        .request(Request::new(
            format!(
                "{}:{}",
                f.control.store.operation_epoch.current(),
                uuid::Uuid::new_v4()
            ),
            Command::ThreadSet(Box::new(ThreadSetArgs {
                thread_id: Some(thread),
                model: Some("changed-model".into()),
                ..Default::default()
            })),
        ))
        .await;
    assert!(changed.ok, "{:?}", changed.error);
    let retried = f
        .control
        .request(Request::new(
            format!("{}:{}", f.control.store.operation_epoch.current(), uuid::Uuid::new_v4()),
            Command::RunRetry(shellboy::protocol::RunRetryArgs::exact(
                f.accepted.run_id.clone().unwrap(),
            )),
        ))
        .await;
    assert!(retried.ok, "{:?}", retried.error);
    let next = f.incoming().await;
    assert_eq!(next.body["model"], "changed-model");
    assert!(!next.body.to_string().contains("DENSE_OLD_CONTEXT"));
    next.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(f.settled().await, "completed");
    f.close().await;
}

/// The first large-input trim runs at a closed-loop boundary (the seeded
/// history ends with a final answer). Once the Turn's own tool loop is open,
/// #182 defers the second trim: transient failures retry unchanged and the
/// deferral is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_turn_defers_the_large_input_trim_while_its_tool_loop_is_open() {
    fn tool(name: &str, command: String) -> Reply {
        Reply::http(200,"application/json",&json!({"id":name,"status":"completed","output":[{
            "type":"function_call","call_id":name,"name":"shell","arguments":json!({"command":command}).to_string()
        }]}).to_string(),"")
    }
    let mut f =
        Fixture::with_history(8, 10, 5000, true, Some(10_000), "protected current request").await;
    for _ in 0..2 {
        f.incoming()
            .await
            .reply
            .send(Reply::error(503, ""))
            .ok()
            .unwrap();
    }
    let reduced = f.incoming().await;
    assert!(!reduced.body.to_string().contains("DENSE_OLD_CONTEXT"));
    reduced
        .reply
        .send(tool(
            "first-work",
            format!("echo FIRST_PROGRESS{}", "x".repeat(10_000)),
        ))
        .ok()
        .unwrap();
    f.incoming_after_tool()
        .await
        .reply
        .send(tool("second-work", "echo SECOND_PROGRESS".into()))
        .ok()
        .unwrap();
    for _ in 0..2 {
        f.incoming_after_tool()
            .await
            .reply
            .send(Reply::error(503, ""))
            .ok()
            .unwrap();
    }
    let reduced_again = f.incoming().await;
    assert!(reduced_again.body.to_string().contains("FIRST_PROGRESS"));
    assert!(reduced_again.body.to_string().contains("SECOND_PROGRESS"));
    assert!(
        reduced_again
            .body
            .to_string()
            .contains("protected current request")
    );
    reduced_again.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(f.settled().await, "completed");
    f.control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM replacement WHERE format_version='context-trim-v1'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert!(
                db.query_row(
                    "SELECT count(*) FROM automatic_compaction WHERE outcome='deferred'",
                    [],
                    |r| r.get::<_, i64>(0)
                )? >= 1
            );
            Ok(())
        })
        .await
        .unwrap();
    f.close().await;
}

/// #185/#182: an overflow inside a huge Turn must not silently drop user
/// input delivered in that Turn. With the tool loop open, the overflow falls
/// back to the active-Turn summary: the request stays in place, the summarizer
/// sees every mid-Turn message in order and must keep its instructions, and a
/// message after the latest round stays on the path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overflow_trim_keeps_every_user_input_of_the_active_turn() {
    fn tool(name: &str, command: String) -> Reply {
        Reply::http(200,"application/json",&json!({"id":name,"status":"completed","output":[{
            "type":"function_call","call_id":name,"name":"shell","arguments":json!({"command":command}).to_string()
        }]}).to_string(),"")
    }
    let mut f = Fixture::with_history(8, 10, 5000, true, None, "TURN_REQUEST_MARKER").await;
    let thread = f.accepted.thread_id.clone();
    let mut sent = Vec::new();
    for (index, work) in ["early-work", "middle-work", "late-work"]
        .into_iter()
        .enumerate()
    {
        let current = f.incoming().await;
        // A leader/user message arrives while the Turn is waiting on the model;
        // it is delivered into this Turn at the next boundary.
        let text = format!("MIDTURN_INSTRUCTION_{index}");
        request(
            &f.control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: Some("leader".into()),
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: text.clone(),
            }),
        )
        .await;
        sent.push(text);
        current
            .reply
            .send(tool(
                work,
                format!("echo {}{}", work.to_uppercase(), "x".repeat(20_000)),
            ))
            .ok()
            .unwrap();
    }
    let overflowing = f.incoming_after_tool().await;
    let body = overflowing.body.to_string();
    for text in &sent {
        assert!(
            body.contains(text.as_str()),
            "{text} was delivered into the Turn before the overflow"
        );
    }
    overflowing
        .reply
        .send(Reply::http(
            400,
            "application/json",
            &json!({"error":{"code":"context_length_exceeded","message":"context limit"}})
                .to_string(),
            "",
        ))
        .ok()
        .unwrap();
    // Map segments and their reduce are tool-free summary requests.
    let mut summaries = 0;
    let mut material = String::new();
    let reduced = loop {
        let next = f.incoming().await;
        if next.body["tools"] != json!([]) {
            break next;
        }
        summaries += 1;
        assert!(summaries <= 8, "bounded summary requests");
        let body = next.body.to_string();
        assert!(body.contains("User instructions to keep"));
        material.push_str(&body);
        next.reply
            .send(Reply::http(
                200,
                "application/json",
                &json!({"id":format!("summary-{summaries}"),"status":"completed","output":[{
                "type":"message","role":"assistant","status":"completed",
                "content":[{"type":"output_text","text":"## Goal\nwork summarized"}]}]})
                .to_string(),
                "",
            ))
            .ok()
            .unwrap();
    };
    assert!(summaries >= 1, "the active-Turn summary ran");
    // Map HTTP requests run concurrently, so arrival order is not source order.
    // Every source must still be presented; durable provenance below checks chronology.
    for marker in [
        "EARLY-WORK",
        "MIDTURN_INSTRUCTION_0",
        "MIDDLE-WORK",
        "MIDTURN_INSTRUCTION_1",
    ] {
        assert!(
            material.contains(marker),
            "summary map inputs omitted {marker}"
        );
    }
    let input = reduced.body["input"].as_array().unwrap().clone();
    let reduced_text = reduced.body.to_string();
    assert!(reduced_text.contains("TURN_REQUEST_MARKER"));
    assert!(
        !reduced_text.contains("EARLY-WORKxxxx"),
        "summarized work proves the reduction ran"
    );
    let notice = input
        .iter()
        .position(|item| {
            item.to_string()
                .contains("tool work below in the active Turn was summarized")
        })
        .expect("active-Turn summary notice");
    assert!(
        input
            .iter()
            .all(|item| item.get("type").is_none_or(|t| t == "message")),
        "no tool call or result remains: {input:?}"
    );
    let request_at = input
        .iter()
        .position(|item| item.to_string().contains("TURN_REQUEST_MARKER"))
        .unwrap();
    // #182 user decision B (simplified): messages delivered during the
    // replaced rounds are carried by the summary (the mock does not echo
    // them); the last message arrived after the latest round and stays.
    assert!(
        input[notice]
            .to_string()
            .contains("2 user message(s) delivered during the summarized work")
    );
    let third = input
        .iter()
        .position(|item| item.to_string().contains("MIDTURN_INSTRUCTION_2"))
        .unwrap();
    assert!(
        request_at < notice && notice < third,
        "order: request, summary, later message: {input:?}"
    );
    reduced.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(f.settled().await, "completed");
    f.control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                3
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM replacement WHERE format_version='context-trim-v1'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM node WHERE kind='summary'", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                1
            );
            let summary_node: String = db.query_row("SELECT id FROM node WHERE kind='summary' ORDER BY rowid DESC LIMIT 1", [], |r| r.get(0))?;
            let ordered_sources = db.prepare("SELECT n.payload_json FROM replacement r JOIN replacement_source s ON s.replacement_id=r.id JOIN node n ON n.id=s.source_node_id WHERE r.output_node_id=?1 ORDER BY s.ordinal")?
                .query_map([summary_node], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let source_text = ordered_sources.join("\n");
            let at = |marker: &str| source_text.find(marker).unwrap_or_else(|| panic!("missing source marker {marker}"));
            assert!(at("EARLY-WORK") < at("MIDTURN_INSTRUCTION_0"));
            assert!(at("MIDTURN_INSTRUCTION_0") < at("MIDDLE-WORK"));
            assert!(at("MIDDLE-WORK") < at("MIDTURN_INSTRUCTION_1"));
            Ok(())
        })
        .await
        .unwrap();
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreducible_explicit_overflow_recovers_without_any_resend() {
    let mut f = Fixture::with_history(8, 10, 5000, true, None, "only protected request").await;
    let first = f.incoming().await;
    let target = f.accepted.thread_id.clone();
    let turn = f.accepted.turn_id.clone().unwrap();
    f.control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let request: String = tx.query_row(
                "SELECT message_node_id FROM receipt WHERE turn_id=?1",
                [turn],
                |r| r.get(0),
            )?;
            let revision: i64 = tx.query_row(
                "SELECT selection_revision FROM thread WHERE id=?1",
                [&target],
                |r| r.get(0),
            )?;
            shellboy::history::select_path(
                &tx,
                &target,
                revision as u64,
                &[request],
                "2026-10-30T00:00:00Z",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    first.reply.send(Reply::http(400,"application/json",&json!({"error":{"code":"context_length_exceeded","message":"explicit capacity rejection"}}).to_string(),"")).ok().unwrap();
    assert_eq!(f.settled().await, "recovery_wait");
    let attempts = f.attempts().await;
    assert_eq!(
        attempts.len(),
        1,
        "no retry is admitted for unchanged explicitly rejected input"
    );
    assert_eq!(
        f.reduction_outcomes().await,
        [Some("no_safe_reduction".into())]
    );
    let detail = f
        .control
        .request(Request::new(
            format!(
                "{}:{}",
                f.control.store.operation_epoch.current(),
                uuid::Uuid::new_v4()
            ),
            Command::TurnGet(TurnTarget {
                turn_id: f.accepted.turn_id.clone().unwrap(),
            }),
        ))
        .await;
    assert!(detail.ok);
    let reason = &detail.result.unwrap()["context"]["recovery_reason"];
    assert_eq!(reason["details"]["automatic_retries"], 0);
    assert_eq!(reason["details"]["automatic_reduction_unavailable"], true);
    assert!(
        reason["message"]
            .as_str()
            .unwrap()
            .contains("no further automatic input reduction")
    );
    assert!(
        f.requests.try_recv().is_err(),
        "no outbound request is queued after terminal recovery"
    );
    f.close().await;
}

async fn usage_limit_new_conversation(f: &mut Fixture) {
    let thread = request(
        &f.control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.accepted = serde_json::from_value(
        request(
            &f.control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread),
                text: "synthetic quota observation".into(),
            }),
        )
        .await,
    )
    .unwrap();
}
fn bounded_field(value: Option<&Value>) -> Value {
    const LIMIT: usize = 160;
    match value {
        Some(Value::String(text)) if text.chars().count() > LIMIT => {
            let head: String = text.chars().take(LIMIT).collect();
            Value::String(format!(
                "{head}...[truncated, {} chars]",
                text.chars().count()
            ))
        }
        Some(Value::Object(_) | Value::Array(_)) => {
            let text = value.unwrap().to_string();
            if text.chars().count() > LIMIT {
                let head: String = text.chars().take(LIMIT).collect();
                Value::String(format!(
                    "{head}...[truncated, {} chars]",
                    text.chars().count()
                ))
            } else {
                value.unwrap().clone()
            }
        }
        Some(other) => other.clone(),
        None => Value::Null,
    }
}

/// Bounded evidence for a one-attempt assertion: the case, the fixture's count
/// of fully read POSTs (requests.jsonl rows are appended only after the whole
/// body was read, so this is distinct from admitted model_attempt rows), and
/// each row's saved classification, HTTP/rate facts and retry disposition.
fn retry_attempt_diagnostics(
    case: &str,
    fixture_posts: usize,
    attempts: &[(String, Option<Value>)],
) -> String {
    use serde_json::json;

    let evidence: Vec<_> = attempts
        .iter()
        .take(8)
        .map(|(state, saved)| {
            let error = saved.as_ref().and_then(|value| value.get("error"));
            let field = |name: &str| saved.as_ref().and_then(|value| value.get(name));
            let disposition = if field("automatic_retry").is_some() {
                "retry_scheduled"
            } else if field("automatic_retry_exhausted").is_some() {
                "retry_exhausted"
            } else if field("automatic_retry_deferred").is_some() {
                "retry_deferred"
            } else if saved.is_some() {
                "no_automatic_retry"
            } else {
                "no_saved_failure"
            };
            json!({
                "state": state,
                "disposition": disposition,
                "classification": {
                    "kind": bounded_field(error.and_then(|value| value.get("kind"))),
                    "message": bounded_field(error.and_then(|value| value.get("message"))),
                    "status": bounded_field(error.and_then(|value| value.get("status"))),
                    "stream_provider_status": bounded_field(error.and_then(|value| value.get("stream_provider_status"))),
                    "automatic_retry": bounded_field(field("automatic_retry")),
                    "automatic_retry_exhausted": bounded_field(field("automatic_retry_exhausted")),
                    "automatic_retry_deferred": bounded_field(field("automatic_retry_deferred")),
                },
                "http_rate": bounded_field(field("rate_observation")),
            })
        })
        .collect();
    serde_json::to_string(&json!({
        "case": bounded_field(Some(&Value::String(case.to_owned()))),
        "fixture_posts_fully_read": fixture_posts,
        "attempt_rows": attempts.len(),
        "attempts": evidence,
        "omitted_attempt_count": attempts.len().saturating_sub(evidence.len()),
    }))
    .unwrap()
}

fn fixture_request_log_lines(f: &Fixture) -> usize {
    std::fs::read_to_string(f._root.path().join("requests.jsonl"))
        .unwrap()
        .lines()
        .count()
}

#[tokio::test]
async fn explicit_usage_limit_recovers_without_resend_blocks_other_thread_and_expires() {
    for shape in 0..8 {
        eprintln!("synthetic usage-limit shape {shape}");
        let mut f = Fixture::new(8, 60_000, 120_000).await;
        let first = f.incoming().await;
        assert_eq!(fixture_request_log_lines(&f), 1);
        let resets_at = chrono::Utc::now().timestamp() + 3600;
        let fact = json!({"error":{"type":"usage_limit_reached","resets_at":resets_at,"limit_window_minutes":10080}});
        let body = match shape {
            0 => fact.to_string(),
            1 => json!({"error":{"message":format!("litellm.RateLimitError: provider - {fact}")}})
                .to_string(),
            2 => {
                json!({"error":{"type":"usage_limit_reached","resets_in_seconds":3600}}).to_string()
            }
            5 => format!("litellm.RateLimitError: provider - {fact}"),
            // Observed LiteLLM wrapping of a plan limit: nested exception
            // names and diagnostic text after the embedded JSON.
            6 => json!({"error":{"message":format!(
                "litellm.RateLimitError: RateLimitError: ChatgptException - {}\nReceived Model Group=m\nAvailable Model Group Fallbacks=None",
                json!({"error":{"type":"usage_limit_reached","plan_type":"prolite","resets_at":resets_at,"limit_window_minutes":10080,"resets_in_seconds":3600}})
            ),"type":null,"code":"429"}})
            .to_string(),
            // Exact provider text reported by the owner, with the reset moved
            // to a fixed offset from now so the test does not expire.
            7 => json!({"error":{"message":format!(
                "provider: litellm.RateLimitError: RateLimitError: ChatgptException - {{\"error\":{{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\",\"plan_type\":\"prolite\",\"resets_at\":{resets_at},\"eligible_promo\":null,\"limit_window_minutes\":10080,\"resets_in_seconds\":408856}}}}. Received Model Group=gpt-6.1-terra\nAvailable Model Group Fallbacks=None"
            )}})
            .to_string(),
            _ => {
                json!({"error":{"type":"rate_limit_error","message":"temporary limit"}}).to_string()
            }
        };
        let header_date = format!(
            "Retry-After: {}\r\n",
            chrono::DateTime::from_timestamp(resets_at, 0)
                .unwrap()
                .to_rfc2822()
        );
        let mut reply = Reply::http(
            429,
            "application/json",
            &body,
            if shape == 3 {
                "Retry-After: 3600\r\n"
            } else if shape == 4 {
                &header_date
            } else {
                ""
            },
        );
        if shape == 3 {
            // The reset is already known at headers. Withhold the advertised
            // body until the client closes; success cannot depend on draining it.
            let end = reply
                .bytes
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .unwrap()
                + 4;
            reply.bytes.truncate(end);
            reply.hold_open = true;
        }
        first.reply.send(reply).ok().unwrap();
        assert_eq!(f.settled().await, "recovery_wait");
        let run = request(
            &f.control,
            Command::RunGet(RunTarget {
                run_id: f.accepted.run_id.clone().unwrap(),
            }),
        )
        .await;
        let reason = &run["context"]["recovery_reason"];
        let limit = &reason["details"]["usage_limit"];
        assert!(limit["resets_at_utc"].as_str().is_some());
        assert_eq!(limit["locally_blocked"], false);
        if !(2..6).contains(&shape) {
            assert_eq!(limit["resets_at"], resets_at);
            assert_eq!(limit["limit_window_minutes"], 10080);
        }
        assert!(
            reason["message"]
                .as_str()
                .unwrap()
                .contains(limit["resets_at_utc"].as_str().unwrap())
        );
        assert!(reason["hint"].as_str().unwrap().contains("/model"));
        let attempts = f.attempts().await;
        assert_eq!(attempts.len(), 1);
        assert!(
            attempts[0]
                .1
                .as_ref()
                .unwrap()
                .get("automatic_retry")
                .is_none()
        );
        usage_limit_new_conversation(&mut f).await;
        assert_eq!(f.settled().await, "recovery_wait");
        assert_eq!(fixture_request_log_lines(&f), 1);
        let run = request(
            &f.control,
            Command::RunGet(RunTarget {
                run_id: f.accepted.run_id.clone().unwrap(),
            }),
        )
        .await;
        assert_eq!(
            run["context"]["recovery_reason"]["details"]["usage_limit"]["locally_blocked"],
            true
        );
        assert!(run["context"]["recovery_reason"]["details"]["http"].is_null());
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(3602)).await;
        tokio::time::resume();
        usage_limit_new_conversation(&mut f).await;
        f.incoming()
            .await
            .reply
            .send(Reply::success())
            .ok()
            .unwrap();
        assert_eq!(f.settled().await, "completed");
        assert_eq!(fixture_request_log_lines(&f), 2);
        f.close().await;
    }
}

#[tokio::test]
async fn short_explicit_usage_limit_keeps_the_normal_retry_schedule() {
    let mut f = Fixture::new(8, 60_000, 120_000).await;
    let reset = chrono::Utc::now().timestamp() + 30;
    let body = json!({"error":{"type":"usage_limit_reached","resets_at":reset}}).to_string();
    f.incoming()
        .await
        .reply
        .send(Reply::http(429, "application/json", &body, ""))
        .ok()
        .unwrap();
    f.incoming()
        .await
        .reply
        .send(Reply::success())
        .ok()
        .unwrap();
    assert_eq!(f.settled().await, "completed");
    assert_eq!(fixture_request_log_lines(&f), 2);
    let attempts = f.attempts().await;
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0].1.as_ref().unwrap()["automatic_retry"]["number"],
        1
    );
    f.close().await;
}

#[tokio::test]
async fn unknown_429_keeps_retry_policy_without_inferred_usage_limit() {
    let mut f = Fixture::new(8, 60_000, 120_000).await;
    // Zero Retry-After keeps ordinary retry eligibility; the shared gate adds its safety floor.
    f.incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 0\r\n"))
        .ok()
        .unwrap();
    f.incoming()
        .await
        .reply
        .send(Reply::success())
        .ok()
        .unwrap();
    assert_eq!(f.settled().await, "completed");
    assert_eq!(fixture_request_log_lines(&f), 2);
    let attempts = f.attempts().await;
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0].1.as_ref().unwrap()["automatic_retry"]["number"],
        1
    );
    assert!(
        attempts[0].1.as_ref().unwrap()["error"]
            .get("usage_limit")
            .is_none()
    );
    f.close().await;
}

/// A documented thinking-binding refusal consumes one shared retry slot for
/// the reasoning-stripped POST. Zero slots permit no resend. A second refusal
/// never resends, even when another slot remains.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thinking_binding_rejection_is_resent_once_without_reasoning_and_recorded() {
    const BINDING: &str = "messages.3.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation. Remove the block, or set `thinking.block_binding.prefix_mismatch_behavior` to \"drop_block\".";
    let binding = || {
        Reply::http(
            400,
            "application/json",
            &json!({"type":"error","error":{"code":"invalid_request_error","message":BINDING}})
                .to_string(),
            "",
        )
    };
    for (slots, second_rejection) in [(0, false), (1, false), (2, true)] {
        let mut f = Fixture::new(slots, 10, 5_000).await;
        let first = f.incoming().await;
        first.reply.send(Reply::http(200,"application/json",&json!({"id":"tool","status":"completed","output":[
            {"type":"reasoning","id":"rs-tool","summary":[],"encrypted_content":"SIGNED"},
            {"type":"function_call","call_id":"call","name":"shell","arguments":json!({"command":"echo BINDING"}).to_string()}
        ]}).to_string(),"")).ok().unwrap();
        let after_tool = f.incoming_after_tool().await;
        assert!(
            after_tool.body.to_string().contains("SIGNED"),
            "reasoning is replayed before the rejection"
        );
        after_tool.reply.send(binding()).ok().unwrap();
        if slots == 0 {
            assert_eq!(f.settled().await, "recovery_wait");
            let attempts = f.attempts().await;
            assert_eq!(
                attempts.len(),
                2,
                "only the tool POST and the rejected POST"
            );
            let error = attempts[1].1.as_ref().unwrap();
            assert_eq!(error["error"]["provider_code"], "invalid_request_error");
            assert_eq!(error["error"]["status"], 400);
            assert!(error.get("thinking_binding_retry").is_none());
            f.no_more_requests().await;
            f.close().await;
            continue;
        }
        let resent = f.incoming().await;
        let input = resent.body["input"].as_array().unwrap();
        assert!(
            input.iter().all(|item| item["type"] != "reasoning"),
            "{input:?}"
        );
        assert!(
            input
                .iter()
                .any(|item| item["type"] == "function_call_output"),
            "tool result kept"
        );
        if second_rejection {
            resent.reply.send(binding()).ok().unwrap();
            assert_eq!(f.settled().await, "recovery_wait");
        } else {
            resent.reply.send(Reply::success()).ok().unwrap();
            assert_eq!(f.settled().await, "completed");
        }
        let attempts = f.attempts().await;
        let recorded: Vec<_> = attempts
            .iter()
            .filter_map(|(_, error)| error.as_ref())
            .filter(|error| error.get("thinking_binding_retry").is_some())
            .collect();
        assert_eq!(recorded.len(), 1, "{attempts:?}");
        assert_eq!(recorded[0]["automatic_retry"]["reason"], "thinking_binding");
        assert_eq!(recorded[0]["automatic_retry"]["number"], 1);
        assert_eq!(recorded[0]["automatic_retry"]["limit"], slots);
        assert_eq!(
            attempts.len(),
            3,
            "tool request, rejected request, one resend"
        );
        let epoch: i64 = f
            .control
            .store
            .database
            .call(|db| Ok(db.query_row("SELECT reasoning_epoch FROM thread", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(epoch, 1);
        f.no_more_requests().await;
        f.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_threads_record_independent_429_observations() {
    let mut fixture = Fixture::new(0, 10, 120_000).await;
    let first = fixture.incoming().await;
    let thread = request(
        &fixture.control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_accepted: MessageAccepted = serde_json::from_value(
        request(
            &fixture.control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread),
                text: "second synthetic thread".into(),
            }),
        )
        .await,
    )
    .unwrap();
    let second = fixture.incoming().await;
    let turn1 = fixture.accepted.turn_id.clone().unwrap();
    first
        .reply
        .send(Reply::error(429, "Retry-After: 0\r\n"))
        .ok()
        .unwrap();
    // Let the first failure's diagnostic finish before the second response;
    // the owner logger deliberately drops records on concurrent lock contention.
    let log = fixture.owner.loaded.data_dir.join("logs/shellboy.log");
    crate::test_watchdog::watchdog("first 429 failure diagnostic", async {
        loop {
            let body = std::fs::read_to_string(&log).unwrap_or_default();
            if body
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|event| {
                    event["event"] == "model_failure"
                        && event["ids"]["turn_id"].as_str() == Some(turn1.as_str())
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "first 429 event missing: {error}; log={:?}; dropped={}",
            std::fs::read_to_string(&log),
            fixture.owner.diagnostics.pending_dropped()
        )
    });
    second.reply.send(Reply::error(429, "")).ok().unwrap();
    let turn2 = second_accepted.turn_id.unwrap();
    let database = fixture.control.store.database.clone();
    crate::test_watchdog::watchdog("concurrent 429 observations", async {
        loop {
            let turns = [turn1.clone(), turn2.clone()];
            let count = database.call(move |db| {
                Ok(db.query_row("SELECT count(*) FROM model_attempt WHERE turn_id IN (?1,?2) AND json_extract(input_manifest_json,'$.rate_observation.status')=429", rusqlite::params![turns[0], turns[1]], |row| row.get::<_, i64>(0))?)
            }).await.unwrap();
            if count == 2 { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("both concurrent attempts must persist their own 429");
    crate::test_watchdog::watchdog("two 429 log events", async {
        loop {
            let body = std::fs::read_to_string(&log).unwrap_or_default();
            let count = body
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|event| event["event"] == "model_rate_limited")
                .count();
            if count >= 2 {
                assert_eq!(count, 2);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "exactly two 429 events must be logged: {error}; log={:?}; dropped={}",
            std::fs::read_to_string(&log),
            fixture.owner.diagnostics.pending_dropped()
        )
    });
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_cooldown_blocks_other_thread_and_stop_remains_responsive() {
    let mut fixture = Fixture::new(0, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 30\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let thread = request(
        &fixture.control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &fixture.control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread),
                text: "second synthetic thread".into(),
            }),
        )
        .await,
    )
    .unwrap();
    crate::test_watchdog::watchdog(
        "connection cooldown stop",
        request(
            &fixture.control,
            Command::RunStop(shellboy::protocol::RunStopTarget::Run {
                run_id: accepted.run_id.unwrap(),
            }),
        ),
    )
    .await
    .expect("stop must not wait for the cooldown");
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_cooldown_revalidates_model_before_second_thread_sends() {
    let mut fixture = Fixture::new(0, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 1\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let thread = request(
        &fixture.control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let _: MessageAccepted = serde_json::from_value(
        request(
            &fixture.control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: "second synthetic thread".into(),
            }),
        )
        .await,
    )
    .unwrap();
    request(
        &fixture.control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread),
            model: Some("changed-model".into()),
            ..Default::default()
        })),
    )
    .await;
    let sent = fixture.incoming().await;
    assert_eq!(sent.body["model"], "changed-model");
    sent.reply.send(Reply::success()).ok().unwrap();
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_summary_waits_for_connection_cooldown_and_shutdown_cancels_it() {
    let mut fixture =
        Fixture::with_history(0, 10, 5_000, true, Some(1_000), "synthetic summary wait").await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 30\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let edit = ContextEditArgs {
        thread_id: Some(fixture.accepted.thread_id.clone()),
        action: ContextEditAction::Summary,
        from_node_id: Some("old-large-context".into()),
        to_node_id: Some("old-answer".into()),
        max_chars: None,
    };
    let accepted = request(&fixture.control, Command::ContextEdit(edit)).await;
    assert!(accepted["operation_id"].is_string());
    let target = fixture.accepted.thread_id.clone();
    crate::test_watchdog::watchdog("manual summary starts", async {
        loop {
            let key = format!("compact.inflight.{target}");
            let active = fixture
                .control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                        [key],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if active {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let thread = fixture.accepted.thread_id.clone();
    let counted: i64 = fixture
        .control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM model_attempt WHERE kind='summary' AND thread_id=?1",
                [thread],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        counted, 0,
        "waiting must not consume a summary request slot"
    );
    let changed = request(
        &fixture.control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(fixture.accepted.thread_id.clone()),
            model: Some("changed-model".into()),
            ..Default::default()
        })),
    );
    crate::test_watchdog::watchdog(
        "manual summary waits for connection cooldown and shutdown cancels it",
        changed,
    )
    .await
    .expect("Thread boundary remains available during admission wait");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_summary_adopts_fixed_r_after_model_change_during_connection_wait() {
    let mut fixture = Fixture::with_history(
        0,
        10,
        5_000,
        true,
        Some(1_000),
        "synthetic summary validation",
    )
    .await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 5\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let target = fixture.accepted.thread_id.clone();
    let accepted = request(
        &fixture.control,
        Command::ContextEdit(ContextEditArgs {
            thread_id: Some(target.clone()),
            action: ContextEditAction::Summary,
            from_node_id: Some("old-large-context".into()),
            to_node_id: Some("old-answer".into()),
            max_chars: None,
        }),
    )
    .await;
    let operation = accepted["operation_id"].as_str().unwrap().to_owned();
    crate::test_watchdog::watchdog("manual summary begins", async {
        loop {
            let key = format!("compact.inflight.{target}");
            let active = fixture
                .control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                        [key],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if active {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        fixture.requests.try_recv().is_err(),
        "the provider summary remains queued behind the shared cooldown"
    );
    crate::test_watchdog::watchdog(
        "Thread boundary stays available during summary admission wait",
        request(
            &fixture.control,
            Command::ThreadSet(Box::new(ThreadSetArgs {
                thread_id: Some(target),
                model: Some("changed-model".into()),
                ..Default::default()
            })),
        ),
    )
    .await
    .unwrap();
    let summary = crate::test_watchdog::watchdog(
        "manual summary proceeds after R-external model change",
        fixture.incoming(),
    )
    .await
    .unwrap();
    assert_eq!(summary.body["tools"], json!([]));
    summary.reply.send(Reply::success()).ok().unwrap();
    let state =
        crate::test_watchdog::watchdog("manual summary adopts after connection wait", async {
            loop {
                let status = request(
                    &fixture.control,
                    Command::OperationGet(OperationTarget {
                        operation_id: operation.clone(),
                    }),
                )
                .await;
                let state = status["downstream"]["state"].as_str().unwrap_or("");
                if state == "failed" || state == "completed" {
                    break state.to_owned();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    assert_eq!(
        state, "completed",
        "R IDs remain authoritative after model change"
    );
    let thread = fixture.accepted.thread_id.clone();
    fixture.control.store.database.call(move |db| {
        let sources: Vec<String> = db.prepare("SELECT s.source_node_id FROM replacement_source s JOIN replacement r ON r.id=s.replacement_id JOIN thread_path p ON p.node_id=r.output_node_id WHERE p.thread_id=?1 AND r.format_version='shellboy-summary-v1' ORDER BY s.ordinal")?
            .query_map([thread], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(sources, ["old-large-context", "old-answer"]);
        Ok(())
    }).await.unwrap();
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_connection_wait_is_recorded_on_successful_attempt() {
    let mut fixture = Fixture::new(0, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 5\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let target = request(
        &fixture.control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let accepted: MessageAccepted = serde_json::from_value(
        request(
            &fixture.control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(target),
                text: "synthetic waited request".into(),
            }),
        )
        .await,
    )
    .unwrap();
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::success())
        .ok()
        .unwrap();
    let turn = accepted.turn_id.unwrap();
    let observation = crate::test_watchdog::watchdog("waited attempt completes", async {
        loop {
            let turn = turn.clone();
            let record = fixture.control.store.database.call(move |db| {
                Ok(db.query_row("SELECT json_extract(input_manifest_json,'$.rate_observation') FROM model_attempt WHERE turn_id=?1 AND state='completed'", [turn], |r| r.get::<_, String>(0)).optional()?)
            }).await.unwrap();
            if let Some(record) = record { break serde_json::from_str::<Value>(&record).unwrap(); }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(observation["status"], 200);
    let admission = observation["admission_wait_ms"].as_u64().unwrap();
    assert!(
        admission > 0,
        "actual gate wait must not be recorded as zero"
    );
    assert_eq!(observation["applied_wait_ms"], admission);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_connection_wait_is_recorded_in_existing_usage() {
    let mut fixture =
        Fixture::with_history(0, 10, 5_000, true, Some(1_000), "summary wait usage").await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 5\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let target = fixture.accepted.thread_id.clone();
    let accepted = request(
        &fixture.control,
        Command::ContextEdit(ContextEditArgs {
            thread_id: Some(target.clone()),
            action: ContextEditAction::Summary,
            from_node_id: Some("old-large-context".into()),
            to_node_id: Some("old-answer".into()),
            max_chars: None,
        }),
    )
    .await;
    let operation = accepted["operation_id"].as_str().unwrap().to_owned();
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::success())
        .ok()
        .unwrap();
    let state = crate::test_watchdog::watchdog("summary usage recorded", async {
        loop {
            let status = request(
                &fixture.control,
                Command::OperationGet(OperationTarget {
                    operation_id: operation.clone(),
                }),
            )
            .await;
            let state = status["downstream"]["state"].as_str().unwrap_or("");
            if state == "failed" || state == "completed" {
                break state.to_owned();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state, "completed");
    let observation: String = fixture.control.store.database.call(move |db| Ok(db.query_row(
        "SELECT json_extract(input_manifest_json,'$.rate_observation') FROM model_attempt WHERE kind='summary' AND thread_id=?1 ORDER BY rowid LIMIT 1",
        [target], |r| r.get(0))?)).await.unwrap();
    let observation: Value = serde_json::from_str(&observation).unwrap();
    assert_eq!(observation["status"], 200);
    let wait = observation["admission_wait_ms"].as_u64().unwrap();
    assert!(wait > 0);
    assert_eq!(observation["applied_wait_ms"], wait);
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_restart_clears_connection_cooldown_without_resuming_old_work() {
    let mut fixture = Fixture::new(0, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 30\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let Fixture {
        _root: root,
        owner,
        control,
        mut requests,
        server,
        ..
    } = fixture;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    drop(control);
    let owner = DataOwner::start(
        Some(root.path().join("config.toml")),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    assert!(
        requests.try_recv().is_err(),
        "restart must not replay old work"
    );
    let thread = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    request(
        &control,
        Command::MessageSend(MessageSendArgs {
            max_model_requests: None,
            resume_recovery: false,

            human: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread),
            text: "explicit post-restart work".into(),
        }),
    )
    .await;
    let next = crate::test_watchdog::watchdog(
        "owner restart clears connection cooldown without resuming old work",
        requests.recv(),
    )
    .await
    .expect("new owner must not retain the old 30-second cooldown")
    .unwrap();
    next.reply.send(Reply::success()).ok().unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_connection_probe_releases_queued_threads_without_serializing_requests() {
    let mut fixture = Fixture::new(0, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(429, "Retry-After: 5\r\n"))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let mut turns = Vec::new();
    for index in 0..3 {
        let thread = request(
            &fixture.control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_thread_id: None,
                connection: None,
                model: None,
                effort: None,
            }),
        )
        .await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let accepted: MessageAccepted = serde_json::from_value(
            request(
                &fixture.control,
                Command::MessageSend(MessageSendArgs {
                    max_model_requests: None,
                    resume_recovery: false,
                    human: false,
                    sender_label: None,
                    deliver: Deliver::Next,
                    thread_id: Some(thread),
                    text: format!("queued synthetic thread {index}"),
                }),
            )
            .await,
        )
        .unwrap();
        turns.push(accepted.turn_id.unwrap());
    }
    let probe = fixture.incoming().await;
    probe.reply.send(Reply::success()).ok().unwrap();
    let earlier = fixture.incoming().await;
    // Keep the earlier request in flight: the next queued Thread must still send.
    let later = fixture.incoming().await;
    earlier.reply.send(Reply::success()).ok().unwrap();
    later.reply.send(Reply::success()).ok().unwrap();
    crate::test_watchdog::watchdog("released Threads complete", async {
        loop {
            let turns = turns.clone();
            let completed: i64 =
                fixture
                    .control
                    .store
                    .database
                    .call(move |db| {
                        Ok(db.query_row(
                "SELECT count(*) FROM turn WHERE id IN (?1,?2,?3) AND state='completed'",
                rusqlite::params![turns[0], turns[1], turns[2]], |r| r.get(0))?)
                    })
                    .await
                    .unwrap();
            if completed == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_limit_stop_matches_wire_in_live_ends_and_saved_usage() {
    const PARTIAL: &str = "abc https://h/p?q=1 synthetic-extra-value";
    let responses = |reason: &str, empty: bool| {
        let output = if empty {
            json!([])
        } else {
            json!([{"type":"message","role":"assistant","status":"incomplete","content":[{"type":"output_text","text":PARTIAL}]}])
        };
        json!({"type":"response.incomplete","response":{
            "id":"limited","status":"incomplete","output":output,"incomplete_details":{"reason":reason}
        }})
    };
    let messages = concat!(
        r#"data: {"type":"message_start","message":{"id":"limited","content":[],"usage":{"input_tokens":5}}}"#,
        "\n\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"abc"}}"#,
        "\n\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\n",
        r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call","name":"shell","input":{}}}"#,
        "\n\n",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#,
        "\n\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":1}}"#,
        "\n\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );
    let chat = concat!(
        r#"data: {"id":"limited","choices":[{"index":0,"delta":{"role":"assistant","content":"abc","tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"shell","arguments":"{\"command\":"}}]},"finish_reason":null}]}"#,
        "\n\n",
        r#"data: {"id":"limited","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );
    for (connection, thread_limit, key, wire_limit, source, reason) in [
        (
            "api='chat_completions'\nmax_tokens=500\n",
            None,
            "max_completion_tokens",
            json!(500),
            Some("connection"),
            "length",
        ),
        (
            "api='anthropic_messages'\n",
            None,
            "max_tokens",
            json!(32768),
            Some("default"),
            "model_context_window_exceeded",
        ),
        (
            "max_tokens=5678\nextra_headers={X-Feature='synthetic-extra-value'}\n",
            Some(1234),
            "max_output_tokens",
            json!(1234),
            Some("thread_override"),
            "max_output_tokens",
        ),
        (
            "api='anthropic_messages'\n",
            None,
            "max_tokens",
            json!(32768),
            Some("default"),
            "max_tokens",
        ),
        (
            "",
            None,
            "max_output_tokens",
            Value::Null,
            None,
            "max_output_tokens",
        ),
        (
            "max_tokens=5678\n",
            None,
            "max_output_tokens",
            json!(5678),
            Some("connection"),
            "content_filter",
        ),
        (
            "extra_body={max_output_tokens=777}\n",
            None,
            "max_output_tokens",
            json!(777),
            Some("extra_body"),
            "max_output_tokens",
        ),
        (
            "extra_body={max_output_tokens='opaque'}\n",
            None,
            "max_output_tokens",
            json!("opaque"),
            None,
            "max_output_tokens",
        ),
        ("", None, "max_output_tokens", Value::Null, None, "eof"),
    ] {
        let settings = FixtureSettings {
            connection,
            max_tokens: thread_limit,
            ..Default::default()
        };
        let mut fixture =
            Fixture::configured(0, 10, 5_000, false, None, "output limit", settings).await;
        let incoming = fixture.incoming().await;
        assert_eq!(incoming.body[key], wire_limit);
        let events = |after| {
            Command::EventsWait(EventsWaitArgs {
                after,
                thread_ids: vec![fixture.accepted.thread_id.clone()],
                timeout_ms: 0,
                include_text: true,
                include_reasoning_activity: false,
                include_output: false,
            })
        };
        let cursor = request(&fixture.control, events(None)).await["cursor"]
            .as_str()
            .unwrap()
            .to_owned();
        let empty = key == "max_output_tokens" && wire_limit.is_null() && reason != "eof";
        let body = if reason == "eof" {
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"abc\"}\n\n".to_owned()
        } else if key == "max_tokens" {
            if reason == "max_tokens" {
                messages.to_owned()
            } else {
                messages
                    .split("\n\n")
                    .filter(|event| !event.contains("\"index\":1"))
                    .collect::<Vec<_>>()
                    .join("\n\n")
                    .replace("max_tokens", reason)
            }
        } else if key == "max_completion_tokens" {
            chat.to_owned()
        } else {
            format!("data: {}\n\n", responses(reason, empty))
        };
        incoming
            .reply
            .send(Reply::http(200, "text/event-stream", &body, ""))
            .ok()
            .unwrap();
        let limited = matches!(reason, "max_output_tokens" | "max_tokens" | "length");
        let settled = fixture.settled().await;
        let expected_state = if limited {
            "completed"
        } else {
            "recovery_wait"
        };
        assert_eq!(settled, expected_state, "{connection:?} / {reason}");
        let result = crate::test_watchdog::watchdog("committed attempt end", async {
            let mut cursor = cursor;
            loop {
                let mut command = events(Some(cursor));
                if let Command::EventsWait(args) = &mut command {
                    args.timeout_ms = 5_000;
                }
                let result = request(&fixture.control, command).await;
                if result["attempt_ends"]
                    .as_array()
                    .is_some_and(|ends| !ends.is_empty())
                {
                    break result;
                }
                assert_ne!(result["status"], "resync_required");
                cursor = result["cursor"].as_str().unwrap().to_owned();
            }
        })
        .await
        .unwrap();
        let ends = result["attempt_ends"].as_array().unwrap();
        assert_eq!(ends.len(), 1, "{result}");
        let end = &ends[0];
        let attempt = end["attempt_id"].as_str().unwrap();
        let usage = request(
            &fixture.control,
            Command::UsageGet(UsageGetArgs {
                attempt_id: Some(attempt.into()),
                ..Default::default()
            }),
        )
        .await;
        if !limited {
            assert_eq!(end["outcome"], "not_adopted");
            assert!(end.get("output_limit").is_none());
            assert_eq!(usage["output_limit"], Value::Null);
        } else {
            let expected = json!({"stop":true,"sent_max_tokens":wire_limit.as_u64(),"source":source,"connection":"fixture"});
            assert_eq!(end["outcome"], "adopted");
            assert_eq!(end["output_limit"], expected);
            if thread_limit.is_some() {
                let attempt = attempt.to_owned();
                let saved: String = fixture
                    .control
                    .store
                    .database
                    .call(move |db| {
                        Ok(db.query_row(
                            "SELECT error_json FROM model_attempt WHERE id=?1",
                            [attempt],
                            |row| row.get(0),
                        )?)
                    })
                    .await
                    .unwrap();
                assert!(saved.contains(PARTIAL), "{saved}");
            }
            let turn = fixture.accepted.turn_id.clone().unwrap();
            let node: String = fixture.control.store.database.call(move |db| {
                let (node, tools, extra): (String, i64, i64) = db.query_row(
                    "SELECT answer_node_id,(SELECT count(*) FROM tool_group WHERE turn_id=u.id), (SELECT count(*) FROM node WHERE turn_id=u.id AND kind='incomplete_response') FROM turn u WHERE id=?1",
                    [turn], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                )?;
                assert_eq!((tools, extra), (0, 0));
                Ok(node)
            }).await.unwrap();
            let history = request(
                &fixture.control,
                Command::HistoryRead(HistoryReadArgs {
                    display: None,
                    node_id: node,
                    end_message_only: false,
                    parts: false,
                    limit_bytes: 4096,
                    cursor: None,
                    encoding: "utf-8".into(),
                }),
            )
            .await;
            assert_eq!(history["provenance"]["output_limit"], expected);
            if empty {
                assert!(
                    history["provenance"].get("attempt_id").is_none(),
                    "no synthetic output item: {history}"
                );
            } else {
                let expected_text = if key == "max_output_tokens" {
                    PARTIAL
                } else {
                    "abc"
                };
                assert!(history.to_string().contains(expected_text), "{history}");
            }
            assert_eq!(usage["output_limit"], expected);
        }
        if thread_limit.is_some() {
            // A historical row cannot reconstruct facts from the override or current configuration.
            let attempt = attempt.to_owned();
            fixture.control.store.database.call({ let attempt = attempt.clone(); move |db| {
                db.execute("UPDATE model_attempt SET input_manifest_json=json_remove(input_manifest_json,'$.output_limit') WHERE id=?1", [&attempt])?;
                Ok(())
            }}).await.unwrap();
            let old = request(
                &fixture.control,
                Command::UsageGet(UsageGetArgs {
                    attempt_id: Some(attempt.into()),
                    ..Default::default()
                }),
            )
            .await;
            assert_eq!(old["output_limit"], Value::Null);
        }
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_limit_does_not_adopt_a_summary() {
    let mut fixture =
        Fixture::with_history(0, 10, 5_000, false, Some(1_000), "summary limit").await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::success())
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "completed");
    let operation = request(
        &fixture.control,
        Command::ContextEdit(ContextEditArgs {
            thread_id: Some(fixture.accepted.thread_id.clone()),
            action: ContextEditAction::Summary,
            from_node_id: Some("old-large-context".into()),
            to_node_id: Some("old-answer".into()),
            max_chars: None,
        }),
    )
    .await;
    let operation_id = operation["operation_id"].as_str().unwrap().to_owned();
    let cut = r#"{"type":"response.incomplete","response":{"id":"cut-summary","status":"incomplete","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"cut summary"}]}],"incomplete_details":{"reason":"max_output_tokens"}}}"#;
    fixture
        .incoming()
        .await
        .reply
        .send(terminal_sse("", cut, ""))
        .ok()
        .unwrap();
    let downstream = crate::test_watchdog::watchdog("cut Summary stays failed", async {
        loop {
            let result = request(
                &fixture.control,
                Command::OperationGet(OperationTarget {
                    operation_id: operation_id.clone(),
                }),
            )
            .await;
            match result["downstream"]["state"].as_str() {
                Some("failed") => break result["downstream"].clone(),
                Some("completed") => panic!("cut Summary adopted: {result}"),
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .unwrap();
    assert!(downstream["outcome"]["error"].is_object(), "{downstream}");
    // The cut summary was not adopted: the selected range is still on the path.
    let thread = fixture.accepted.thread_id.clone();
    fixture.control.store.database.call(move |db| {
        let kept: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id='old-large-context')",
            [thread],
            |r| r.get(0),
        )?;
        assert!(kept);
        Ok(())
    }).await.unwrap();
    fixture.close().await;
}

const TERMINAL_SSE_500: &str = r#"{"error":{"code":"500","message":"fixture"}}"#;

fn terminal_sse(prefix: &str, event: &str, headers: &str) -> Reply {
    Reply::http(
        200,
        "text/event-stream",
        &format!("{prefix}data: {event}\n\n"),
        headers,
    )
}

fn assert_terminal_stream_failure(failure: &Value, witness: Option<u16>) {
    assert_eq!(failure["error"]["kind"], "provider");
    assert_eq!(failure["rate_observation"]["status"], 200);
    assert!(failure["error"]["status"].is_null());
    assert!(
        failure["error"]
            .get("strict_untyped_stream_error_before_done")
            .is_none()
    );
    if let Some(code) = witness {
        assert_eq!(
            failure["error"]["stream_provider_status"],
            json!({"value":code,"source":"stream_error_code"})
        );
    } else {
        assert!(failure["error"].get("stream_provider_status").is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_untyped_terminal_after_full_response_and_reasoning_item_retries() {
    let response = json!({
        "id": "resp_fixture", "object": "response", "created_at": 1,
        "model": "fixture", "status": "in_progress", "output": [],
        "error": null, "incomplete_details": null, "usage": null,
        "instructions": "Fixture instructions", "tools": [],
        "tool_choice": "auto", "parallel_tool_calls": true,
        "reasoning": {"effort": "medium", "summary": null},
        "text": {"format": {"type": "text"}}, "metadata": {},
        "temperature": 1.0, "top_p": 1.0, "max_output_tokens": null,
        "previous_response_id": null, "store": false, "truncation": "disabled"
    });
    let created = json!({"type": "response.created", "sequence_number": 0, "response": &response});
    let in_progress =
        json!({"type": "response.in_progress", "sequence_number": 1, "response": &response});
    let reasoning = json!({
        "type": "response.output_item.added", "sequence_number": 2, "output_index": 0,
        "item": {"id": "rs_fixture", "type": "reasoning", "summary": []}
    });
    let prefix = format!("data: {created}\n\ndata: {in_progress}\n\ndata: {reasoning}\n\n");
    for code in [500, 429] {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        let first = fixture.incoming().await;
        let event = json!({"error":{
            "code":code.to_string(), "message":"Fixture upstream closed",
            "param":"None", "type":"None"
        }});
        first
            .reply
            .send(terminal_sse(&prefix, &event.to_string(), ""))
            .ok()
            .unwrap();
        let second = fixture.incoming().await;
        assert_eq!(second.body, first.body);
        // Observe the committed attempt while the second request still awaits its reply.
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].0, "failed");
        let failure = attempts[0].1.as_ref().unwrap();
        assert_terminal_stream_failure(failure, Some(code));
        assert_eq!(failure["raw_response"], event);
        assert_eq!(failure["partial_text"], "");
        assert_eq!(failure["partial_reasoning"], "");
        assert_eq!(failure["completed_items"], json!({}));
        assert_eq!(failure["rate_observation"]["retry_after_present"], false);
        assert_eq!(
            failure["automatic_retry"],
            json!({"number":1,"limit":2,"delay_ms":10,"partial_response_discarded":true,
                "possible_duplicate_charge":true})
        );
        second.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(fixture.settled().await, "completed");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[1].0, "completed");
        assert!(attempts[1].1.is_none());
        assert_eq!(fixture_request_log_lines(&fixture), 2);
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untyped_terminal_sse_retry_fails_closed_for_typed_or_nontransient_errors() {
    let collision = r#"{"type":"response.failed","response":{"output":[{"type":"reasoning"}]},"response":{"error":{"code":"500","message":"fixture"}}}"#;
    let done_prefix = "data: [DONE]\n\n";
    for (prefix, event, witness) in [
        ("", collision, Some(500)),
        (
            "",
            r#"{"type":"response.failed","response":{"output":[{"type":"reasoning"}],"error":{"code":"500"}}}"#,
            Some(500),
        ),
        (done_prefix, TERMINAL_SSE_500, Some(500)),
        ("", r#"{"type":"error","error":{"code":"500"}}"#, Some(500)),
        // The extra root `response` field makes this unlike #315's strict
        // one-key `{error: {code, ...}}` retry envelope.
        (
            "",
            r#"{"response":{"status":"in_progress"},"error":{"code":"500"}}"#,
            Some(500),
        ),
        (
            "",
            r#"{"error":{"code":"500","message":{"wrapped":"fixture"}}}"#,
            Some(500),
        ),
        ("", r#"{"error":{"code":"400"}}"#, Some(400)),
        ("", r#"{"error":{"code":"401"}}"#, Some(401)),
        ("", r#"{"error":{"code":"403"}}"#, Some(403)),
        ("", r#"{"error":{"code":"408"}}"#, Some(408)),
        ("", r#"{"error":{"code":500}}"#, None),
        ("", r#"{"error":{"message":"HTTP 500"}}"#, None),
        ("", r#"{"error":{"code":"0500"}}"#, None),
        ("", r#"{"error":{"code":"400","code":"500"}}"#, None),
        (
            "",
            r#"{"error":{"code":"400"},"error":{"code":"500"}}"#,
            None,
        ),
        (
            "",
            r#"{"type":"other","type":"error","error":{"code":"500"}}"#,
            None,
        ),
    ] {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        fixture
            .incoming()
            .await
            .reply
            .send(terminal_sse(prefix, event, ""))
            .ok()
            .unwrap();
        assert_eq!(fixture.settled().await, "recovery_wait", "{prefix}{event}");
        let attempts = fixture.attempts().await;
        assert_eq!(
            attempts.len(),
            1,
            "terminal classification for prefix={prefix:?} event={event}; bounded attempt diagnostics={}",
            retry_attempt_diagnostics(
                &format!("prefix={prefix:?} event={event}"),
                fixture_request_log_lines(&fixture),
                &attempts
            )
        );
        let failure = attempts[0].1.as_ref().unwrap();
        assert_terminal_stream_failure(failure, witness);
        if event == collision {
            assert_eq!(
                failure["raw_response"],
                serde_json::from_str::<Value>(TERMINAL_SSE_500).unwrap()
            );
            assert_eq!(failure["partial_reasoning"], "");
            assert_eq!(failure["completed_items"], json!({}));
        }
        assert!(failure.get("automatic_retry").is_none());
        assert!(failure.get("automatic_retry_exhausted").is_none());
        assert_eq!(fixture_request_log_lines(&fixture), 1);
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untyped_terminal_sse_does_not_retry_after_invalid_json() {
    let prefix = "data: {\"error\":{\"code\":\"599\"}\n\n";
    let mut fixture = Fixture::new(2, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(terminal_sse(prefix, TERMINAL_SSE_500, ""))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 1);
    let failure = attempts[0].1.as_ref().unwrap();
    assert_eq!(failure["error"]["kind"], "protocol");
    assert_eq!(failure["rate_observation"]["status"], 200);
    // Invalid JSON fails before the following terminal can supply a witness.
    assert!(failure["error"].get("stream_provider_status").is_none());
    assert!(failure["raw_response"].is_null());
    assert!(failure.get("automatic_retry").is_none());
    assert!(failure.get("automatic_retry_exhausted").is_none());
    assert_eq!(fixture_request_log_lines(&fixture), 1);
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untyped_terminal_sse_retry_does_not_apply_to_http_200_json() {
    let mut fixture = Fixture::new(2, 10, 5_000).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::http(200, "application/json", TERMINAL_SSE_500, ""))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 1);
    let failure = attempts[0].1.as_ref().unwrap();
    assert_eq!(failure["rate_observation"]["status"], 200);
    assert_eq!(failure["error"]["provider_code"], "500");
    assert!(failure["error"].get("stream_provider_status").is_none());
    assert!(failure.get("automatic_retry").is_none());
    assert_eq!(fixture_request_log_lines(&fixture), 1);
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untyped_terminal_sse_uses_the_same_budget_including_zero_and_exhaustion() {
    for limit in [0, 2] {
        let mut fixture = Fixture::new(limit, 10, 5_000).await;
        for _ in 0..=limit {
            fixture
                .incoming()
                .await
                .reply
                .send(terminal_sse("", TERMINAL_SSE_500, ""))
                .ok()
                .unwrap();
        }
        assert_eq!(fixture.settled().await, "recovery_wait");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), limit as usize + 1);
        for (index, (_, error)) in attempts.iter().enumerate() {
            let failure = error.as_ref().unwrap();
            assert_terminal_stream_failure(failure, Some(500));
            if index < limit as usize {
                assert_eq!(failure["automatic_retry"]["number"], index + 1);
                assert_eq!(failure["automatic_retry"]["limit"], limit);
                assert_eq!(failure["automatic_retry"]["delay_ms"], 10);
            } else {
                assert!(failure.get("automatic_retry").is_none());
                assert_eq!(
                    failure.get("automatic_retry_exhausted").cloned(),
                    (limit > 0).then(|| json!({"retries":limit,"limit":limit}))
                );
            }
        }
        assert_eq!(fixture_request_log_lines(&fixture), limit as usize + 1);
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn untyped_terminal_sse_honors_real_retry_after_and_stop() {
    for seconds in [30, 601] {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        let header = format!("Retry-After: {seconds}\r\n");
        fixture
            .incoming()
            .await
            .reply
            .send(terminal_sse("", TERMINAL_SSE_500, &header))
            .ok()
            .unwrap();
        if seconds == 30 {
            fixture.waiting_for_retry().await;
            crate::test_watchdog::watchdog(
                "tests/response_retries.rs: untyped_terminal_sse_honors_real_retry_after_and_stop",
                request(
                    &fixture.control,
                    Command::RunStop(shellboy::protocol::RunStopTarget::Run {
                        run_id: fixture.accepted.run_id.clone().unwrap(),
                    }),
                ),
            )
            .await
            .expect("stop did not finish within fixture watchdog");
            assert_eq!(fixture.settled().await, "stopped");
        } else {
            assert_eq!(fixture.settled().await, "recovery_wait");
        }
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 1);
        let failure = attempts[0].1.as_ref().unwrap();
        assert_terminal_stream_failure(failure, Some(500));
        assert_eq!(
            failure["rate_observation"]["retry_after_ms"],
            seconds * 1_000
        );
        if seconds == 30 {
            assert_eq!(failure["automatic_retry"]["delay_ms"], 30_000);
        } else {
            assert_eq!(
                failure["automatic_retry_deferred"]["retry_after_ms"],
                601_000
            );
        }
        assert_eq!(fixture_request_log_lines(&fixture), 1);
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_terminal_sse_error_does_not_retry_and_preserves_partial_facts() {
    let mut fixture = Fixture::new(2, 1, 5_000).await;
    let partial = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_text.delta","delta":"visible partial text"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"partial-call","name":"shell","arguments":"{\"command\":\"echo never\"}"}}),
        json!({"type":"response.failed","response":{"error":{"code":"503","message":"SECRET_STREAM_ERROR"}}}),
    );
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::partial(&partial, false))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(
        attempts.len(),
        1,
        "typed terminal stream failure cannot retry"
    );
    let failure = attempts[0].1.as_ref().unwrap();
    assert_eq!(failure["partial_text"], "visible partial text");
    assert_eq!(failure["completed_items"]["0"]["call_id"], "partial-call");
    assert_eq!(failure["error"]["stream_provider_status"]["value"], 503);
    assert_eq!(
        failure["error"]["stream_provider_status"]["source"],
        "stream_error_code"
    );
    let tools: i64 = fixture
        .control
        .store
        .database
        .call(|db| Ok(db.query_row("SELECT count(*) FROM shell_execution", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(
        tools, 0,
        "partial completed tool items are recorded but never executed"
    );
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn genuine_eof_partial_output_retries_and_never_executes_saved_tools() {
    let mut fixture = Fixture::new(2, 1, 5_000).await;
    let body = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_text.delta","delta":"S1B_PARTIAL_SENTINEL"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"S1B_PARTIAL_CALL","name":"shell","arguments":"{\"command\":\"echo never\"}"}}),
    );
    for _ in 0..3 {
        let request = fixture.incoming().await;
        assert!(!request.body.to_string().contains("S1B_PARTIAL_SENTINEL"));
        assert!(!request.body.to_string().contains("S1B_PARTIAL_CALL"));
        request
            .reply
            .send(Reply::http(200, "text/event-stream", &body, ""))
            .ok()
            .unwrap();
    }
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 3);
    let last = attempts.last().unwrap().1.as_ref().unwrap();
    assert_eq!(last["partial_text"], "S1B_PARTIAL_SENTINEL");
    assert_eq!(last["completed_items"]["0"]["call_id"], "S1B_PARTIAL_CALL");
    let tool_executions: i64 = fixture
        .control
        .store
        .database
        .call(|db| Ok(db.query_row("SELECT count(*) FROM shell_execution", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(
        tool_executions, 0,
        "partial completed tool items are retained but not run"
    );
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test]
async fn conversation_and_summary_commit_each_transient_retry_before_dispatch() {
    let mut fixture =
        Fixture::with_history(1, 10, 5_000, false, Some(1_000), "shared request policy").await;
    for kind in ["conversation", "summary"] {
        let operation = if kind == "summary" {
            let accepted = request(
                &fixture.control,
                Command::ContextEdit(ContextEditArgs {
                    thread_id: Some(fixture.accepted.thread_id.clone()),
                    action: ContextEditAction::Summary,
                    from_node_id: Some("old-large-context".into()),
                    to_node_id: Some("old-answer".into()),
                    max_chars: None,
                }),
            )
            .await;
            Some(accepted["operation_id"].as_str().unwrap().to_owned())
        } else {
            None
        };
        let mut previous = None;
        let mut first_body = None;
        for ordinal in 1..=2 {
            let incoming = fixture.incoming().await;
            if let Some(first) = &first_body {
                assert_eq!(
                    first, &incoming.body,
                    "{kind} transient retry must resend the same input"
                );
            } else {
                first_body = Some(incoming.body.clone());
            }
            let thread = fixture.accepted.thread_id.clone();
            let ids = fixture.control.store.database.call(move |db| {
                Ok(db.prepare("SELECT id FROM model_attempt WHERE thread_id=?1 AND kind=?2 ORDER BY rowid")?
                    .query_map(rusqlite::params![thread,kind], |row| row.get::<_,String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            }).await.unwrap();
            assert_eq!(
                ids.len(),
                ordinal,
                "{kind} dispatch {ordinal} requires a committed attempt"
            );
            assert_ne!(previous, ids.last().cloned());
            previous = ids.last().cloned();
            incoming
                .reply
                .send(if ordinal == 1 {
                    Reply::error(500, "")
                } else {
                    Reply::success()
                })
                .ok()
                .unwrap();
        }
        if let Some(operation) = operation {
            crate::test_watchdog::watchdog(
                "shared request policy Summary terminal result",
                async {
                    loop {
                        let status = request(
                            &fixture.control,
                            Command::OperationGet(OperationTarget {
                                operation_id: operation.clone(),
                            }),
                        )
                        .await;
                        match status["downstream"]["state"].as_str() {
                            Some("completed") => break,
                            Some("failed") => panic!("Summary did not adopt: {status}"),
                            _ => tokio::task::yield_now().await,
                        }
                    }
                },
            )
            .await
            .unwrap();
        } else {
            assert_eq!(fixture.settled().await, "completed");
        }
    }
    fixture.no_more_requests().await;
    fixture.close().await;
}

impl Fixture {
    async fn park_at_usage_limit(&mut self) -> chrono::DateTime<chrono::Utc> {
        self.park_at_usage_limit_after(6).await
    }

    async fn park_at_usage_limit_after(&mut self, seconds: i64) -> chrono::DateTime<chrono::Utc> {
        let first = self.incoming().await;
        let reset =
            chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + seconds, 0).unwrap();
        let body = usage_limit_body(reset);
        first
            .reply
            .send(Reply::http(429, "application/json", &body, ""))
            .ok()
            .unwrap();
        assert_eq!(self.settled().await, "recovery_wait");
        assert_eq!(fixture_request_log_lines(self), 1);
        reset
    }

    async fn restart_after(self, at: chrono::DateTime<chrono::Utc>) -> Self {
        let Self {
            _root,
            owner,
            control,
            requests,
            server,
            accepted,
        } = self;
        control.shutdown().await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
        wait_for_reset(at).await;
        let owner = DataOwner::start(
            Some(_root.path().join("config.toml")),
            Some(_root.path().join("data")),
            _root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, _root.path().into());
        control.start().await.unwrap();
        Self {
            _root,
            owner,
            control,
            requests,
            server,
            accepted,
        }
    }

    async fn assert_usage_limit_admissions(&self, expected: i64) {
        self.owner.db.call(move |db| {
            let count: i64 = db.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='recovery_resumed' AND json_extract(source_json,'$.kind')='owner'", [], |row| row.get(0))?;
            assert_eq!(count, expected);
            Ok(())
        }).await.unwrap();
    }
}

async fn wait_for_reset(at: chrono::DateTime<chrono::Utc>) {
    while let Ok(remaining) = (at - chrono::Utc::now()).to_std() {
        tokio::time::sleep(remaining).await;
    }
}

#[tokio::test]
async fn owner_maintenance_resends_usage_limit_once_at_saved_reset() {
    let mut f = Fixture::new(0, 100, 120_000).await;
    f.control.start().await.unwrap();
    let reset = f.park_at_usage_limit().await;
    let resumed = f.incoming().await;
    assert!(
        chrono::Utc::now() >= reset,
        "owner resent before saved reset"
    );
    resumed.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(f.settled().await, "completed");
    assert_eq!(fixture_request_log_lines(&f), 2);
    f.assert_usage_limit_admissions(1).await;
    f.close().await;
}

#[tokio::test]
async fn owner_start_rediscovers_overdue_usage_limit_once() {
    let mut f = Fixture::new(0, 100, 120_000).await;
    // Do not start maintenance on the first owner: the first admission must be
    // driven by the replacement owner's scan, not an old in-memory signal.
    let reset = f.park_at_usage_limit().await;
    let mut f = f.restart_after(reset).await;
    let resumed = f.incoming().await;
    resumed.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(f.settled().await, "completed");
    f.assert_usage_limit_admissions(1).await;
    let mut f = f.restart_after(reset).await;
    assert_eq!(f.settled().await, "completed");
    f.no_more_requests().await;
    assert_eq!(fixture_request_log_lines(&f), 2);
    f.assert_usage_limit_admissions(1).await;
    f.close().await;
}

#[tokio::test]
async fn owner_with_usage_limit_retry_disabled_stays_parked_past_reset() {
    let mut f = Fixture::configured(
        0,
        100,
        120_000,
        true,
        None,
        "synthetic opt-out",
        FixtureSettings {
            auto_retry: Some(false),
            ..Default::default()
        },
    )
    .await;
    f.control.start().await.unwrap();
    let reset = f.park_at_usage_limit().await;
    wait_for_reset(reset + chrono::Duration::seconds(1)).await;
    assert_eq!(f.settled().await, "recovery_wait");
    f.no_more_requests().await;
    assert_eq!(fixture_request_log_lines(&f), 1);
    f.assert_usage_limit_admissions(0).await;
    f.close().await;
}

fn usage_limit_body(reset: chrono::DateTime<chrono::Utc>) -> String {
    json!({"error":{"type":"usage_limit_reached","resets_at":reset.timestamp()}}).to_string()
}

async fn human_usage_limit_resume(held: usize) {
    let mut f = Fixture::new(0, 100, 120_000).await;
    f.control.start().await.unwrap();
    let reset = f.park_at_usage_limit_after(12).await;
    let thread = f.accepted.thread_id.clone();
    let send = |human, text| {
        Command::MessageSend(MessageSendArgs {
            max_model_requests: None,
            human,
            text,
            thread_id: Some(thread.clone()),
            resume_recovery: false,
            sender_label: None,
            deliver: Deliver::Next,
        })
    };
    for index in 0..held {
        let queued: MessageAccepted = serde_json::from_value(
            request(&f.control, send(false, format!("Held input {index}"))).await,
        )
        .unwrap();
        assert_eq!(
            queued.delivery_status,
            shellboy::protocol::DeliveryStatus::Held
        );
    }
    if held <= 128 {
        // Expire the connection's monotonic cooldown without moving the saved
        // UTC reset, so the resumed attempt can receive the same reset from HTTP.
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(13)).await;
        tokio::time::resume();
    }
    request(&f.control, send(true, "Resume before reset".into())).await;
    assert!(
        chrono::Utc::now() < reset,
        "human must resume before the reset"
    );
    let expected_requests = if held > 128 {
        assert_eq!(f.settled().await, "recovery_wait");
        let turn = f.accepted.turn_id.clone().unwrap();
        f.owner.db.call(move |db| {
            let remaining: i64 = db.query_row("SELECT count(*) FROM receipt WHERE turn_id=?1 AND state='held' AND hold_reason='recovery_wait'", [turn], |row| row.get(0))?;
            assert_eq!(remaining, 1);
            Ok(())
        }).await.unwrap();
        1
    } else {
        let resumed = f.incoming().await;
        assert!(
            chrono::Utc::now() < reset,
            "explicit resume must reach the provider before reset"
        );
        resumed
            .reply
            .send(Reply::http(
                429,
                "application/json",
                &usage_limit_body(reset),
                "",
            ))
            .ok()
            .unwrap();
        assert_eq!(f.settled().await, "recovery_wait");
        2
    };
    wait_for_reset(reset + chrono::Duration::seconds(1)).await;
    assert_eq!(f.settled().await, "recovery_wait");
    f.no_more_requests().await;
    assert_eq!(fixture_request_log_lines(&f), expected_requests);
    f.assert_usage_limit_admissions(0).await;
    f.close().await;
}

#[tokio::test]
async fn human_usage_limit_resume_consumes_reset_even_with_held_receipts_remaining() {
    human_usage_limit_resume(129).await;
}

#[tokio::test]
async fn human_usage_limit_resume_does_not_rearm_after_same_provider_reset() {
    human_usage_limit_resume(0).await;
}

#[tokio::test]
async fn model_filter_changes_catalog_not_direct_model_execution() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut requests, server) = proxy(root.path().join("requests.jsonl")).await;
    let path = root.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            r#"
        default_connection = 'gpt'
        default_model = 'gpt-4'
        [embedding]
        connection = 'gpt'
        model = 'embed-test'
        [connections.gpt]
        base_url = {url:?}
        models.include = ['gpt-*']
        models.exclude = ['*-preview']
        [connections.claude]
        base_url = {url:?}
        models.include = ['claude-?']
        [connections.raw]
        base_url = {url:?}
        [connections.empty]
        base_url = {url:?}
        models.include = ['no-match-*']
    "#
        ),
    )
    .unwrap();
    let owner = DataOwner::start(
        Some(path),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    for (connection, diagnostic, expected) in [
        ("gpt", false, vec!["gpt-4"]),
        ("claude", false, vec!["claude-x"]),
        (
            "raw",
            false,
            vec!["GPT-4", "claude-x", "gpt-4", "gpt-4-preview"],
        ),
        ("empty", false, vec![]),
        (
            "gpt",
            true,
            vec!["GPT-4", "claude-x", "gpt-4", "gpt-4-preview"],
        ),
    ] {
        let command = if diagnostic {
            Command::ConnectionTest(ConnectionTestArgs {
                connection: Some(connection.into()),
                ..Default::default()
            })
        } else {
            Command::ModelList(ModelListArgs {
                connection: Some(connection.into()),
                thread_id: None,
            })
        };
        let respond = async {
            let incoming = requests.recv().await.unwrap();
            assert!(incoming.body.is_null(), "catalog must be a GET");
            let catalog = json!({"data":[{"id":"gpt-4-preview"},{"id":"gpt-4"},{"id":"GPT-4"},{"id":"claude-x"}]});
            incoming
                .reply
                .send(Reply::http(
                    200,
                    "application/json",
                    &catalog.to_string(),
                    "",
                ))
                .ok()
                .unwrap();
        };
        let (result, ()) = tokio::join!(request(&control, command), respond);
        assert_eq!(result["models"], json!(expected), "{connection}");
    }
    let config = request(&control, Command::ConfigGet(ConfigGetArgs {})).await;
    let changed = request(
        &control,
        Command::ConfigSet(ConfigSetArgs {
            expected_hash: Some(config["file"]["content_hash"].as_str().unwrap().into()),
            changes: vec![
                ConfigChange {
                    field: "connections.gpt.models.exclude".into(),
                    value: Some(json!([])),
                },
                ConfigChange {
                    field: "connections.raw.models.include".into(),
                    value: Some(json!(["gpt-*"])),
                },
            ],
        }),
    )
    .await;
    assert_eq!(changed["written"], true);
    assert_eq!(changed["restart_required"], true);
    let changes = changed["changes"].as_array().unwrap();
    assert!(
        changes
            .iter()
            .any(|entry| entry["field"] == "connections.gpt.models.exclude"
                && entry["restart_required"] == true)
    );
    assert!(
        changes
            .iter()
            .any(|entry| entry["field"] == "connections.raw.models.include"
                && entry["restart_required"] == false)
    );
    for (connection, expected) in [
        ("gpt", vec!["gpt-5"]),
        ("raw", vec!["gpt-4-preview", "gpt-5"]),
    ] {
        let respond = async {
            let incoming = requests.recv().await.unwrap();
            assert!(incoming.body.is_null());
            incoming
                .reply
                .send(Reply::http(
                    200,
                    "application/json",
                    r#"{"data":[{"id":"gpt-5"},{"id":"gpt-4-preview"},{"id":"claude-z"}]}"#,
                    "",
                ))
                .ok()
                .unwrap();
        };
        let (listed, ()) = tokio::join!(
            request(
                &control,
                Command::ModelList(ModelListArgs {
                    connection: Some(connection.into()),
                    thread_id: None
                })
            ),
            respond
        );
        assert_eq!(
            listed["models"],
            json!(expected),
            "fresh provider list with the effective connection settings"
        );
    }
    let thread = request(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: Some("gpt".into()),
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    request(
        &control,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            model: Some("gpt-4-preview".into()),
            ..Default::default()
        })),
    )
    .await;
    let (accepted, ()) = tokio::join!(
        request(
            &control,
            Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread),
                text: "synthetic direct model".into(),
            })
        ),
        async {
            let incoming = crate::test_watchdog::watchdog("direct model POST", async {
                loop {
                    let incoming = requests.recv().await.unwrap();
                    if incoming.body["model"] != "embed-test" {
                        break incoming;
                    }
                    let input = incoming.body["input"].as_array().expect("embedding inputs");
                    let data: Vec<_> = input
                        .iter()
                        .enumerate()
                        .map(|(index, _)| json!({"index":index,"embedding":[1.0,2.0]}))
                        .collect();
                    incoming
                        .reply
                        .send(Reply::http(
                            200,
                            "application/json",
                            &json!({"data":data}).to_string(),
                            "",
                        ))
                        .ok()
                        .unwrap();
                }
            })
            .await
            .unwrap();
            assert_eq!(incoming.body["model"], "gpt-4-preview");
            incoming.reply.send(Reply::success()).ok().unwrap();
        }
    );
    let fixture = Fixture {
        _root: root,
        owner,
        control,
        requests,
        server,
        accepted: serde_json::from_value(accepted).unwrap(),
    };
    assert_eq!(fixture.settled().await, "completed");
    fixture.close().await;
}

#[tokio::test]
async fn model_request_ceiling_stops_before_resending() {
    let mut f = Fixture::configured(
        2,
        1,
        5_000,
        false,
        None,
        "one request",
        FixtureSettings {
            max_model_requests: std::num::NonZeroU32::new(1),
            ..Default::default()
        },
    )
    .await;
    let first = f.incoming().await;
    first.reply.send(Reply::error(500, "")).ok().unwrap();
    assert_eq!(f.settled().await, "stopped");
    let run = request(
        &f.control,
        Command::RunGet(RunTarget {
            run_id: f.accepted.run_id.clone().unwrap(),
        }),
    )
    .await;
    assert_eq!(run["state"], "stopped");
    assert_eq!(
        run["stop_reason"],
        json!({"kind":"model_request_ceiling_reached","requests_used":1})
    );
    assert_eq!(f.attempts().await.len(), 1);
    f.no_more_requests().await;
    f.close().await;
}

#[tokio::test]
async fn model_request_ceiling_allows_normal_completion() {
    let mut f = Fixture::configured(
        2,
        1,
        5_000,
        false,
        None,
        "two available",
        FixtureSettings {
            max_model_requests: std::num::NonZeroU32::new(2),
            ..Default::default()
        },
    )
    .await;
    f.incoming()
        .await
        .reply
        .send(Reply::success())
        .ok()
        .unwrap();
    assert_eq!(f.settled().await, "completed");
    let run = request(
        &f.control,
        Command::RunGet(RunTarget {
            run_id: f.accepted.run_id.clone().unwrap(),
        }),
    )
    .await;
    assert_eq!(run["model_requests"], json!({"ceiling":2,"used":1}));
    assert!(run.get("stop_reason").is_none());
    f.close().await;
}

#[tokio::test]
async fn model_request_ceiling_reservation_survives_owner_restart() {
    let mut f = Fixture::configured(
        2,
        1,
        5_000,
        false,
        None,
        "interrupt first request",
        FixtureSettings {
            max_model_requests: std::num::NonZeroU32::new(2),
            ..Default::default()
        },
    )
    .await;
    let first = f.incoming().await;
    let Fixture {
        _root: root,
        owner,
        control,
        mut requests,
        server,
        accepted,
    } = f;
    crate::test_watchdog::watchdog("restart owner with reserved request", async {
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        drop(control);
        drop(first);
        let owner = DataOwner::start(
            Some(root.path().join("config.toml")),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        control.start().await.unwrap();
        let run = request(
            &control,
            Command::RunGet(RunTarget {
                run_id: accepted.run_id.unwrap(),
            }),
        )
        .await;
        assert_eq!(run["state"], "stopped");
        assert_eq!(run["model_requests"], json!({"ceiling":2,"used":1}));
        assert!(requests.try_recv().is_err());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    })
    .await
    .unwrap();
    server.abort();
}
