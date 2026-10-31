//! Automatic Responses retries through a real owner, using only synthetic loopback traffic.
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

async fn proxy() -> (String, mpsc::Receiver<Incoming>, JoinHandle<()>) {
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
                    connections.spawn(async move {
                        let body = read_request(&mut stream).await;
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
        let (url, requests, server) = proxy().await;
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
                    sender_label: None,
                    now: false,
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
        let retry = &attempts[0].1.as_ref().unwrap()["automatic_retry"];
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
    for limit in [0, 2] {
        let mut fixture = Fixture::new(limit, 10, 5_000).await;
        for _ in 0..=limit {
            fixture
                .incoming()
                .await
                .reply
                .send(Reply::error(503, ""))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_permanent_error_body_is_not_retried() {
    for status in [400, 401, 403, 422] {
        let mut fixture = Fixture::new(2, 10, 500).await;
        fixture.incoming().await.reply.send(Reply {
            bytes: format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: 99999\r\nConnection: close\r\n\r\n{{").into_bytes(),
            hold_open:true,
        }).ok().unwrap();
        assert_eq!(fixture.settled().await, "recovery_wait", "status {status}");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 1);
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
    assert_eq!(fixture.attempts().await.len(), 1);
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
        .send(Reply::error(429, "Retry-After: 601\r\n"))
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
                max_percent: None,
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
            Command::ThreadSet(ThreadSetArgs {
                thread_id: Some(thread),
                model: Some("changed-model".into()),
                ..Default::default()
            }),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_turn_can_reduce_again_after_successful_tool_work() {
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
    assert!(!reduced_again.body.to_string().contains("FIRST_PROGRESS"));
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
                2
            );
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
