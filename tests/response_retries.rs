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

impl Fixture {
    async fn new(retries: u32, max_delay_ms: u64, idle_ms: u64) -> Self {
        Self::with_partial_policy(retries, max_delay_ms, idle_ms, true).await
    }

    async fn with_partial_policy(
        retries: u32,
        max_delay_ms: u64,
        idle_ms: u64,
        partial: bool,
    ) -> Self {
        Self::with_history(
            retries,
            max_delay_ms,
            idle_ms,
            partial,
            None,
            true,
            None,
            "synthetic retry regression",
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn with_history(
        retries: u32,
        max_delay_ms: u64,
        idle_ms: u64,
        partial: bool,
        hint: Option<u64>,
        trim_enabled: bool,
        seed_chars: Option<usize>,
        text: &str,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (url, requests, server) = proxy(root.path().join("requests.jsonl")).await;
        let config = root.path().join("config.toml");
        let _ = hint;
        let trim_settings = format!("auto_trim_on_disconnect={trim_enabled}\n");
        std::fs::write(
            &config,
            format!(
                "default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms={max_delay_ms}\nstream_idle_timeout_ms={idle_ms}\nresponse_header_timeout_ms={idle_ms}\nresponse_retry_partial={partial}\n{trim_settings}"
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
                parent_id: None,
                connection: None,
                model: None,
                effort: None,
            }),
        )
        .await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
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
                    resume_recovery: false,
                    human: false,
                    to_human: false,
                    expect_reply: false,
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
            let rows = db.prepare("SELECT state,error_json FROM model_attempt WHERE turn_id=?1 ORDER BY rowid")?
                .query_map([turn], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows.into_iter().map(|(state, error)| (state, error.map(|value| serde_json::from_str(&value).unwrap()))).collect())
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
                        // retry_or_recover publishes Changed after the automatic_retry
                        // transaction commits, before releasing its boundary/waiting.
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
            ("attempts", "SELECT json_object('id',substr(id,1,128),'state',substr(state,1,64),'error_kind',substr(json_extract(error_json,'$.error.kind'),1,128),'http_phase',substr(json_extract(error_json,'$.error.http_observation.phase'),1,128),'retry_number',json_extract(error_json,'$.automatic_retry.number'),'retry_delay_ms',json_extract(error_json,'$.automatic_retry.delay_ms')) FROM model_attempt WHERE (?1 IS NULL OR turn_id=?1) ORDER BY rowid DESC LIMIT 8"),
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
                format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
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
        assert!(
            first_error["rate_observation"]["applied_wait_ms"]
                .as_u64()
                .is_some()
        );
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
        assert_eq!(error["rate_observation"]["applied_wait_ms"], 0);
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
async fn stalled_empty_or_partial_response_retries_but_partial_optout_waits_for_recovery() {
    for (body, partial, should_retry) in [
        ("", false, true),
        (": heartbeat\n\n", false, true),
        (
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"timeout-partial\"}\n\n",
            true,
            true,
        ),
        (
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"timeout-partial\"}\n\n",
            false,
            false,
        ),
    ] {
        let mut fixture = Fixture::with_partial_policy(2, 10, 300, partial).await;
        fixture
            .incoming()
            .await
            .reply
            .send(Reply::partial(body, true))
            .ok()
            .unwrap();
        if should_retry {
            fixture
                .incoming()
                .await
                .reply
                .send(Reply::success())
                .ok()
                .unwrap();
            assert_eq!(fixture.settled().await, "completed");
            assert_eq!(fixture.attempts().await.len(), 2);
        } else {
            assert_eq!(fixture.settled().await, "recovery_wait");
            assert_eq!(fixture.attempts().await.len(), 1);
        }
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
            loop {
                if let EngineEvent::ModelReceive { thread_id, .. } = events.recv().await.unwrap()
                    && thread_id == fixture.accepted.thread_id
                {
                    break;
                }
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
            usage_limit_log_lines(&fixture),
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
            Command::RunStop(RunTarget {
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
    assert!(rate["applied_wait_ms"].as_u64().is_some());
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
    for partial_policy in [true, false] {
        let mut fixture = Fixture::with_partial_policy(2, 10, 5_000, partial_policy).await;
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
            .send(Reply::partial(&body, false))
            .ok()
            .unwrap();
        if partial_policy {
            let next = fixture.incoming().await;
            assert!(!next.body.to_string().contains("paid-partial-fixture"));
            assert!(!next.body.to_string().contains("must_not_execute"));
            next.reply.send(Reply::success()).ok().unwrap();
            assert_eq!(fixture.settled().await, "completed");
        } else {
            assert_eq!(fixture.settled().await, "recovery_wait");
        }
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
        let mut f = Fixture::with_history(
            8,
            10,
            500,
            true,
            None,
            true,
            Some(10_000),
            "current request",
        )
        .await;
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
        assert!(
            attempts[0]
                .1
                .as_ref()
                .unwrap()
                .get("automatic_trim_attempt")
                .is_none()
        );
        assert_eq!(
            attempts[1].1.as_ref().unwrap()["automatic_retry"]["number"],
            2
        );
        assert_eq!(
            attempts[1].1.as_ref().unwrap()["automatic_trim_attempt"]["outcome"],
            "adopted"
        );
        f.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reduced_and_unreducible_inputs_share_the_eight_retry_budget() {
    for old in [None, Some(10_000)] {
        let mut f = Fixture::with_history(
            8,
            10,
            5000,
            true,
            None,
            true,
            old,
            "current protected input",
        )
        .await;
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
                format!("{}:{}", f.control.store.instance_id, uuid::Uuid::new_v4()),
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
    let mut f = Fixture::with_history(
        1,
        10,
        5000,
        true,
        None,
        true,
        Some(10_000),
        "current request",
    )
    .await;
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
        let mut f =
            Fixture::with_history(3, 10, 5000, true, None, enabled, Some(10_000), "current").await;
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
    let mut f = Fixture::with_history(0, 10, 5000, true, None, true, Some(10_000), "current").await;
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
            format!("{}:{}", f.control.store.instance_id, uuid::Uuid::new_v4()),
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
                    format!("{}:{}", f.control.store.instance_id, uuid::Uuid::new_v4()),
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
            format!("{}:{}", f.control.store.instance_id, uuid::Uuid::new_v4()),
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
            format!("{}:{}", f.control.store.instance_id, uuid::Uuid::new_v4()),
            Command::RunRetry(RunTarget {
                run_id: f.accepted.run_id.clone().unwrap(),
            }),
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
    let mut f = Fixture::with_history(
        8,
        10,
        5000,
        true,
        None,
        true,
        Some(10_000),
        "protected current request",
    )
    .await;
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
                    "SELECT count(*) FROM model_attempt WHERE json_extract(error_json,'$.automatic_trim_attempt.outcome')='deferred'",
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
    let mut f =
        Fixture::with_history(8, 10, 5000, true, None, true, None, "TURN_REQUEST_MARKER").await;
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
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
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
    let mut f = Fixture::with_history(
        8,
        10,
        5000,
        true,
        None,
        true,
        None,
        "only protected request",
    )
    .await;
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
        attempts[0].1.as_ref().unwrap()["automatic_trim_attempt"]["outcome"],
        "no_safe_reduction"
    );
    let detail = f
        .control
        .request(Request::new(
            format!("{}:{}", f.control.store.instance_id, uuid::Uuid::new_v4()),
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
            parent_id: None,
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
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
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
fn usage_limit_log_lines(f: &Fixture) -> usize {
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
        assert_eq!(usage_limit_log_lines(&f), 1);
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
        assert_eq!(usage_limit_log_lines(&f), 1);
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
        assert_eq!(usage_limit_log_lines(&f), 2);
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
    assert_eq!(usage_limit_log_lines(&f), 2);
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
    assert_eq!(usage_limit_log_lines(&f), 2);
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

/// #182 rule 4: the documented thinking-binding rejection is resent once with
/// every earlier reasoning item dropped (new epoch), even with no ordinary
/// retries configured, and the resend is recorded on the failed attempt. A
/// second rejection is not resent again.
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
    for second_rejection in [false, true] {
        let mut f = Fixture::new(0, 10, 5_000).await;
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
            parent_id: None,
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
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
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
            parent_id: None,
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
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread),
                text: "second synthetic thread".into(),
            }),
        )
        .await,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            fixture.requests.recv()
        )
        .await
        .is_err(),
        "second thread must not send during the shared cooldown"
    );
    crate::test_watchdog::watchdog(
        "connection cooldown stop",
        request(
            &fixture.control,
            Command::RunStop(RunTarget {
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
            parent_id: None,
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
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.clone()),
                text: "second synthetic thread".into(),
            }),
        )
        .await,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            fixture.requests.recv()
        )
        .await
        .is_err()
    );
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
    let mut fixture = Fixture::with_history(
        0,
        10,
        5_000,
        true,
        None,
        true,
        Some(1_000),
        "synthetic summary wait",
    )
    .await;
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
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            fixture.requests.recv()
        )
        .await
        .is_err(),
        "summary must not send during shared cooldown"
    );
    let thread = fixture.accepted.thread_id.clone();
    let counted: i64 = fixture
        .control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
        "SELECT COALESCE(SUM(json_extract(value,'$.requests')),0) FROM app_meta WHERE key LIKE ?1",
        [format!("summary.usage.{thread}.%")], |r| r.get(0))?)
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
    tokio::time::timeout(std::time::Duration::from_secs(2), changed)
        .await
        .expect("Thread boundary remains available during admission wait");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_summary_revalidates_selection_after_connection_wait() {
    let mut fixture = Fixture::with_history(
        0,
        10,
        5_000,
        true,
        None,
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
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            fixture.requests.recv()
        )
        .await
        .is_err()
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
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
    .expect("waiting compaction must not hold the Thread boundary");
    let state = crate::test_watchdog::watchdog("manual summary rejects stale selection", async {
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
        state, "failed",
        "changed model must invalidate the pending summary"
    );
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
            parent_id: None,
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
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(target),
                text: "synthetic waited request".into(),
            }),
        )
        .await,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            fixture.requests.recv()
        )
        .await
        .is_err()
    );
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
    let mut fixture = Fixture::with_history(
        0,
        10,
        5_000,
        true,
        None,
        true,
        Some(1_000),
        "summary wait usage",
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
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            fixture.requests.recv()
        )
        .await
        .is_err()
    );
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
        "SELECT json_extract(value,'$.rate_observations[0]') FROM app_meta WHERE key LIKE ?1 ORDER BY key LIMIT 1",
        [format!("summary.usage.{target}.%")], |r| r.get(0))?)).await.unwrap();
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
            parent_id: None,
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
            resume_recovery: false,

            human: false,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread),
            text: "explicit post-restart work".into(),
        }),
    )
    .await;
    let next = tokio::time::timeout(std::time::Duration::from_secs(10), requests.recv())
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
                parent_id: None,
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
                    resume_recovery: false,
                    human: false,
                    to_human: false,
                    expect_reply: false,
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
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            fixture.requests.recv()
        )
        .await
        .is_err(),
        "only one probe may run before its result"
    );
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
async fn terminal_sse_error_vetoes_partial_retries_but_preserves_partial_facts() {
    for retry_partial in [false, true] {
        let mut fixture = Fixture::with_partial_policy(2, 1, 5_000, retry_partial).await;
        let partial = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\n",
            json!({"type":"response.output_text.delta","delta":"visible partial text"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"partial-call","name":"shell","arguments":"{\"command\":\"echo never\"}"}}),
            json!({"error":{"code":"400","message":"SECRET_STREAM_ERROR"}}),
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
            "terminal stream failure cannot retry under either partial policy"
        );
        let failure = attempts[0].1.as_ref().unwrap();
        assert_eq!(failure["partial_text"], "visible partial text");
        assert_eq!(failure["completed_items"]["0"]["call_id"], "partial-call");
        assert_eq!(failure["error"]["stream_provider_status"]["value"], 400);
        assert_eq!(
            failure["error"]["stream_provider_status"]["source"],
            "stream_error_code"
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
        assert_eq!(
            tools, 0,
            "partial completed tool items are recorded but never executed"
        );
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn genuine_eof_partial_output_obeys_retry_partial_and_never_executes_saved_tools() {
    for (retry_partial, expected_attempts) in [(false, 1usize), (true, 3usize)] {
        let mut fixture = Fixture::with_partial_policy(2, 1, 5_000, retry_partial).await;
        let body = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"type":"response.output_text.delta","delta":"S1B_PARTIAL_SENTINEL"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"S1B_PARTIAL_CALL","name":"shell","arguments":"{\"command\":\"echo never\"}"}}),
        );
        for _ in 0..expected_attempts {
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
        assert_eq!(attempts.len(), expected_attempts);
        let last = attempts.last().unwrap().1.as_ref().unwrap();
        assert_eq!(last["partial_text"], "S1B_PARTIAL_SENTINEL");
        assert_eq!(last["completed_items"]["0"]["call_id"], "S1B_PARTIAL_CALL");
        let tool_executions: i64 = fixture
            .control
            .store
            .database
            .call(|db| {
                Ok(db.query_row("SELECT count(*) FROM shell_execution", [], |row| row.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(
            tool_executions, 0,
            "partial completed tool items are retained but not run"
        );
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}
