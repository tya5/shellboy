//! Automatic Responses retries through a real owner, using only synthetic loopback traffic.
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    control::{Control, EngineEvent},
    protocol::*,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

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
        let trim_settings = format!(
            "auto_trim_on_disconnect={trim_enabled}\n{}",
            hint.map(|hint| format!("compact_hint_tokens={hint}\n"))
                .unwrap_or_default()
        );
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
        self.incoming_with_timeout(60)
    }

    #[track_caller]
    fn incoming_after_tool(&mut self) -> impl std::future::Future<Output = Incoming> + '_ {
        self.incoming_with_timeout(60)
    }

    #[track_caller]
    fn incoming_with_timeout(
        &mut self,
        seconds: u64,
    ) -> impl std::future::Future<Output = Incoming> + '_ {
        let caller = std::panic::Location::caller();
        async move {
            match timeout(Duration::from_secs(seconds), self.requests.recv()).await {
                Ok(Some(value)) => value,
                result => {
                    let diagnostic = timeout(
                        Duration::from_secs(5),
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
            let result = timeout(Duration::from_secs(60), async {
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
                    let diagnostic =
                        timeout(Duration::from_secs(5), progress(&self.control, Some(turn))).await;
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
            let result = timeout(Duration::from_secs(60), async {
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
            })
            .await;
            if result.is_err() {
                let diagnostic = timeout(
                    Duration::from_secs(5),
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
        assert!(
            timeout(Duration::from_millis(50), self.requests.recv())
                .await
                .is_err(),
            "unexpected HTTP replay"
        );
    }

    #[track_caller]
    fn close(self) -> impl std::future::Future<Output = ()> {
        let caller = std::panic::Location::caller();
        async move {
            match timeout(Duration::from_secs(60), self.control.shutdown()).await {
                Ok(result) => result.unwrap(),
                Err(_) => {
                    let diagnostic = timeout(
                        Duration::from_secs(5),
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
            match timeout(Duration::from_secs(60), self.owner.shutdown()).await {
                Ok(result) => result.unwrap(),
                Err(_) => {
                    let diagnostic = timeout(
                        Duration::from_secs(5),
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
        let response = match timeout(
            Duration::from_secs(60),
            control.request(Request::new(
                format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                command,
            )),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => {
                let diagnostic = timeout(Duration::from_secs(5), progress(control, None)).await;
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
    // A transient failure before compaction belongs to the same pending chain.
    pending.reply.send(Reply::error(503, "")).ok().unwrap();
    let pending = fixture.incoming().await;
    pending.reply.send(Reply::http(
        400,
        "application/json",
        &json!({"error":{"code":"context_length_exceeded","message":"synthetic context pressure"}}).to_string(),
        "",
    )).ok().unwrap();
    let spilled = fixture.incoming().await;
    assert!(spilled.body["input"].as_array().unwrap().iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call-1"
            && serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap()["omitted"] == true
    }));
    spilled.reply.send(Reply::error(503, "")).ok().unwrap();
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::error(503, ""))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(
        attempts
            .iter()
            .map(|(state, _)| state.as_str())
            .collect::<Vec<_>>(),
        [
            "failed",
            "completed",
            "failed",
            "completed",
            "failed",
            "completed",
            "failed",
            "failed",
            "failed",
            "incomplete"
        ]
    );
    for index in [0, 2, 4, 6] {
        assert_eq!(
            attempts[index].1.as_ref().unwrap()["automatic_retry"]["number"],
            1
        );
    }
    assert_eq!(
        attempts[8].1.as_ref().unwrap()["automatic_retry"]["number"],
        2
    );
    assert_eq!(
        attempts[9].1.as_ref().unwrap()["automatic_retry_exhausted"],
        json!({"retries":2,"limit":2})
    );
    let turn = fixture.accepted.turn_id.clone().unwrap();
    fixture
        .control
        .store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |row| row
                    .get::<_, i64>(0))?,
                3
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM tool_result", [], |row| row
                    .get::<_, i64>(0))?,
                3
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM replacement WHERE format_version='spill-v1'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                2 // Three tool groups leave two old groups eligible for spill.
            );
            assert_eq!(
                db.query_row("SELECT generation FROM turn WHERE id=?1", [turn], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
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
        Reply::error(501, ""),
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
    for status in [400, 401, 403, 501] {
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
    timeout(
        Duration::from_secs(2),
        request(
            &fixture.control,
            Command::RunStop(RunTarget {
                run_id: fixture.accepted.run_id.clone().unwrap(),
            }),
        ),
    )
    .await
    .expect("stop waited for retry delay");
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
    timeout(Duration::from_secs(2), fixture.control.shutdown())
        .await
        .expect("shutdown waited for retry delay")
        .unwrap();
    assert_eq!(fixture.attempts().await.len(), 1);
    fixture.no_more_requests().await;
    timeout(Duration::from_secs(5), fixture.owner.shutdown())
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
    let at = std::time::Instant::now();
    first
        .reply
        .send(Reply::error(429, "Retry-After: 1\r\n"))
        .ok()
        .unwrap();
    let second = fixture.incoming().await;
    assert!(
        at.elapsed() >= Duration::from_millis(900),
        "server wait was shortened"
    );
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
#[ignore = "real 61-second loopback outage; no external API or user data"]
async fn default_retry_policy_survives_a_minute_long_proxy_outage() {
    let defaults = shellboy::config::Connection::default();
    let mut fixture = Fixture::new(
        defaults.response_max_retries,
        defaults.response_retry_max_delay_ms,
        5_000,
    )
    .await;
    let unavailable_until = std::time::Instant::now() + Duration::from_secs(61);
    loop {
        let incoming = fixture.incoming_with_timeout(70).await;
        if std::time::Instant::now() >= unavailable_until {
            incoming.reply.send(Reply::success()).ok().unwrap();
            break;
        }
        incoming.reply.send(Reply::error(503, "")).ok().unwrap();
    }
    assert_eq!(fixture.settled().await, "completed");
    let attempts = fixture.attempts().await;
    assert!(attempts.len() >= 7);
    assert_eq!(attempts.last().unwrap().0, "completed");
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_large_failure_maximally_trims_before_using_retry_budget() {
    for mode in ["500", "501", "503", "disconnect", "header_timeout"] {
        let mut fixture = Fixture::with_history(
            8,
            10,
            1_000,
            true,
            None,
            true,
            Some(400_000),
            "synthetic retry regression",
        )
        .await;
        let first = fixture.incoming().await;
        assert!(
            first.body.to_string().contains("DENSE_OLD_CONTEXT"),
            "no hint must mean no proactive trim"
        );
        let target = fixture.accepted.thread_id.clone();
        let turn = fixture.accepted.turn_id.clone().unwrap();
        let (expected_sources, retained_request, retained_latest) = fixture.control.store.database.call(move |db| {
            let request: String = db.query_row("SELECT message_node_id FROM receipt WHERE turn_id=?1 AND state='accepted'", [turn], |row| row.get(0))?;
            let path = db.prepare("SELECT n.id,n.kind FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 ORDER BY p.ordinal")?
                .query_map([target], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let latest = path.last().unwrap().0.clone();
            let sources = path.into_iter().filter(|(id, _)| id != &request && id != &latest).collect::<Vec<_>>();
            assert_eq!(&sources[..2], &[("old-large-context".into(), "user_message".into()), ("old-answer".into(), "assistant_response".into())]);
            assert_eq!(sources.len(), 5, "fixture includes three eligible context observations in addition to its two seeded nodes");
            for (id, kind) in &sources[2..] {
                assert_eq!(kind, "event");
                let event_kind: String = db.query_row("SELECT e.kind FROM node n JOIN event e ON e.id=json_extract(n.payload_json,'$.event_id') WHERE n.id=?1", [id], |row| row.get(0))?;
                assert_eq!(event_kind, "context_changed");
            }
            Ok((sources, request, latest))
        }).await.unwrap();
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
        first.reply.send(reply).ok().unwrap();
        let trimmed = fixture.incoming().await;
        let body = trimmed.body.to_string();
        assert!(!body.contains("DENSE_OLD_CONTEXT"));
        assert!(body.contains("synthetic retry regression"));
        assert!(body.contains("Restoring a large range may reproduce the same failure"));
        assert!(body.contains(&format!(
            "rag search --scope history:{} --mode keyword",
            fixture.accepted.thread_id
        )));
        assert_eq!(
            trimmed.body["tools"].as_array().unwrap().len(),
            1,
            "trim must not invoke a summary model"
        );
        trimmed.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(fixture.settled().await, "completed", "{mode}");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 2);
        let saved = attempts[0].1.as_ref().unwrap();
        assert!(saved.get("automatic_retry").is_none());
        assert_eq!(saved["automatic_trim_trigger"]["retries_used"], 0);
        assert!(saved["automatic_trim"]["before_tokens"].as_u64().unwrap() > 75_000);
        assert!(saved["automatic_trim"]["target_tokens"].is_null());
        assert_eq!(
            saved["automatic_trim"]["omitted_nodes"],
            expected_sources.len()
        );
        assert_eq!(
            saved["automatic_trim"]["compact"]["original_node_ids"],
            json!(
                expected_sources
                    .iter()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>()
            )
        );
        fixture
            .control
            .store
            .database
            .call(move |db| {
                let actual_sources = db.prepare("SELECT n.id,n.kind FROM replacement_source s JOIN replacement r ON r.id=s.replacement_id JOIN node n ON n.id=s.source_node_id WHERE r.format_version='context-trim-v1' ORDER BY s.ordinal")?
                    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(actual_sources, expected_sources, "all and only the exact eligible original IDs/kinds must be omitted");
                for retained in [retained_request, retained_latest] {
                    assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path WHERE node_id=?1)", [retained], |row| row.get::<_, bool>(0))?);
                }
                let original: String = db.query_row(
                    "SELECT payload_json FROM node WHERE id='old-large-context'",
                    [],
                    |row| row.get(0),
                )?;
                assert!(original.contains("DENSE_OLD_CONTEXT"));
                assert!(original.len() > 400_000);
                assert_eq!(
                    db.query_row("SELECT count(*) FROM shell_execution", [], |row| row
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_input_or_disabled_trim_keeps_ordinary_retries() {
    for (chars, enabled) in [(80_000, true), (400_000, false)] {
        let mut fixture = Fixture::with_history(
            8,
            10,
            5_000,
            true,
            None,
            enabled,
            Some(chars),
            "synthetic retry regression",
        )
        .await;
        fixture
            .incoming()
            .await
            .reply
            .send(Reply::error(503, ""))
            .ok()
            .unwrap();
        let retried = fixture.incoming().await;
        assert!(retried.body.to_string().contains("DENSE_OLD_CONTEXT"));
        retried.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(fixture.settled().await, "completed");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 2);
        assert_eq!(
            attempts[0].1.as_ref().unwrap()["automatic_retry"]["number"],
            1
        );
        assert!(
            attempts[0]
                .1
                .as_ref()
                .unwrap()
                .get("automatic_trim")
                .is_none()
        );
        let saved = attempts[0].1.as_ref().unwrap();
        assert!(
            !saved["error"]["message"]
                .as_str()
                .unwrap()
                .contains("automatic context trim"),
            "small requests must not gain trim-noise in their ordinary error"
        );
        fixture
            .control
            .store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row("SELECT count(*) FROM replacement", [], |row| row
                        .get::<_, i64>(0))?,
                    0,
                    "ordinary retries must issue neither summary nor omission replacements"
                );
                assert!(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM thread_path WHERE node_id='old-large-context')",
                    [],
                    |row| row.get::<_, bool>(0)
                )?);
                let original: String = db.query_row(
                    "SELECT payload_json FROM node WHERE id='old-large-context'",
                    [],
                    |row| row.get(0),
                )?;
                assert!(original.contains("DENSE_OLD_CONTEXT"));
                assert!(original.len() > chars);
                Ok(())
            })
            .await
            .unwrap();
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proactive_trim_requires_explicit_hint_and_precedes_the_first_attempt() {
    for hint in [None, Some(2_000)] {
        let mut fixture = Fixture::with_history(
            8,
            10,
            5_000,
            true,
            hint,
            true,
            Some(80_000),
            "synthetic retry regression",
        )
        .await;
        let first = fixture.incoming().await;
        assert_eq!(
            first.body.to_string().contains("DENSE_OLD_CONTEXT"),
            hint.is_none()
        );
        assert_eq!(fixture.attempts().await.len(), 1);
        first.reply.send(Reply::success()).ok().unwrap();
        assert_eq!(fixture.settled().await, "completed");
        let expected = i64::from(hint.is_some());
        fixture
            .control
            .store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM replacement WHERE format_version='context-trim-v1'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    expected
                );
                Ok(())
            })
            .await
            .unwrap();
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adopted_trim_allows_one_resumed_request_then_failure_enters_recovery() {
    for hint in [None, Some(2_000)] {
        let mut fixture = Fixture::with_history(
            8,
            10,
            5_000,
            true,
            hint,
            true,
            Some(400_000),
            "synthetic retry regression",
        )
        .await;
        let count = if hint.is_some() { 1 } else { 2 };
        for index in 0..count {
            let incoming = fixture.incoming().await;
            assert_eq!(
                incoming.body.to_string().contains("DENSE_OLD_CONTEXT"),
                hint.is_none() && index == 0
            );
            incoming.reply.send(Reply::error(503, "")).ok().unwrap();
        }
        assert_eq!(fixture.settled().await, "recovery_wait");
        assert_eq!(fixture.attempts().await.len(), count);
        fixture
            .control
            .store
            .database
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM replacement WHERE format_version='context-trim-v1'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
        fixture.no_more_requests().await;
        fixture.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_unprotected_groups_follow_ordinary_retries_without_replacement() {
    for path_shrinks_after_post in [false, true] {
        for succeeds in [false, true] {
            no_candidates_after_failure(path_shrinks_after_post, succeeds, 3).await;
        }
        no_candidates_after_failure(path_shrinks_after_post, false, 0).await;
    }
}

async fn no_candidates_after_failure(path_shrinks_after_post: bool, succeeds: bool, retries: u32) {
    let protected_request = "protected current request ".repeat(20_000);
    let mut fixture = Fixture::with_history(
        retries,
        10,
        5_000,
        true,
        None,
        true,
        path_shrinks_after_post.then_some(400_000),
        if path_shrinks_after_post {
            "small protected current request"
        } else {
            &protected_request
        },
    )
    .await;
    let mut events = fixture.control.subscribe();
    let pending = fixture.incoming().await;
    let target = fixture.accepted.thread_id.clone();
    let turn = fixture.accepted.turn_id.clone().unwrap();
    // A real owner appends preparation/context observations. Prepare the precise
    // all-protected selected-path fixture explicitly, retaining those immutable
    // observation nodes outside the path rather than pretending they do not exist.
    let protected_path = fixture
        .control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let request: String = tx.query_row(
                "SELECT message_node_id FROM receipt WHERE turn_id=?1 AND state='accepted'",
                [turn],
                |row| row.get(0),
            )?;
            let revision: i64 = tx.query_row(
                "SELECT selection_revision FROM thread WHERE id=?1",
                [&target],
                |row| row.get(0),
            )?;
            let path = vec![request];
            shellboy::history::select_path(
                &tx,
                &target,
                revision as u64,
                &path,
                "2026-10-01T00:00:02Z",
            )?;
            tx.commit()?;
            Ok(path)
        })
        .await
        .unwrap();
    pending.reply.send(Reply::error(503, "")).ok().unwrap();
    for retry in 0..retries {
        let next = fixture.incoming().await;
        next.reply
            .send(if succeeds && retry + 1 == retries {
                Reply::success()
            } else {
                Reply::error(503, "")
            })
            .ok()
            .unwrap();
    }
    assert_eq!(
        fixture.settled().await,
        if succeeds {
            "completed"
        } else {
            "recovery_wait"
        }
    );
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), retries as usize + 1);
    let failed_count = if succeeds { retries } else { retries + 1 };
    for (number, (_, body)) in attempts.iter().take(failed_count as usize).enumerate() {
        let body = body.as_ref().unwrap();
        if !path_shrinks_after_post || number == 0 {
            assert_eq!(
                body["automatic_trim_attempt"]["outcome"],
                "no_safe_reduction"
            );
        }
        if number < retries as usize {
            assert_eq!(body["automatic_retry"]["number"], number + 1);
        }
    }
    let saved = attempts[0].1.as_ref().unwrap();
    assert_eq!(
        saved["automatic_trim_attempt"]["outcome"],
        "no_safe_reduction"
    );
    assert!(
        saved["automatic_trim_attempt"]["trigger_estimated_tokens"]
            .as_u64()
            .unwrap()
            > 75_000
    );
    if path_shrinks_after_post {
        assert!(
            saved["automatic_trim_attempt"]["result"]["before_tokens"]
                .as_u64()
                .unwrap()
                < 75_000,
            "classification must use the failed request trigger, not this smaller current projection"
        );
    }
    assert!(saved["automatic_trim_attempt"]["result"]["target_tokens"].is_null());
    let target = fixture.accepted.thread_id.clone();
    if !succeeds {
        timeout(Duration::from_secs(2), async {
            loop {
                if let shellboy::control::EngineEvent::Changed {
                    thread_id,
                    recovery: true,
                } = events.recv().await.unwrap()
                    && thread_id == target
                {
                    break;
                }
            }
        })
        .await
        .expect("committed recovery must notify the frontend");
    }
    fixture
        .control
        .store
        .database
        .call(move |db| {
            let actual = db
                .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !succeeds {
                assert_eq!(
                    actual, protected_path,
                    "the failing no-candidate trim must not alter the prepared path"
                );
            }
            assert_eq!(
                db.query_row("SELECT count(*) FROM replacement", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_context_limit_stays_on_the_existing_overflow_path_without_a_hint() {
    let mut fixture = Fixture::with_history(
        1,
        10,
        5_000,
        true,
        None,
        true,
        Some(80_000),
        "synthetic retry regression",
    )
    .await;
    fixture.incoming().await.reply.send(Reply::http(400, "application/json",
        &json!({"error":{"code":"context_length_exceeded","message":"synthetic context overflow"}}).to_string(), "")).ok().unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 1);
    let error = attempts[0].1.as_ref().unwrap();
    assert_eq!(error["error"]["provider_code"], "context_length_exceeded");
    assert!(error.get("automatic_trim").is_none());
    assert!(error.get("automatic_trim_attempt").is_none());
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reduced_request_still_honors_a_permitted_retry_after() {
    let mut fixture = Fixture::with_history(
        8,
        10,
        5_000,
        true,
        None,
        true,
        Some(400_000),
        "synthetic retry regression",
    )
    .await;
    let first = fixture.incoming().await;
    let at = std::time::Instant::now();
    first
        .reply
        .send(Reply::error(503, "Retry-After: 1\r\n"))
        .ok()
        .unwrap();
    let reduced = fixture.incoming().await;
    assert!(at.elapsed() >= Duration::from_millis(900));
    assert!(!reduced.body.to_string().contains("DENSE_OLD_CONTEXT"));
    reduced.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(fixture.settled().await, "completed");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0].1.as_ref().unwrap()["automatic_trim_trigger"]["retry_after_ms"],
        1_000
    );
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_trim_resumption_restores_normal_retries_and_later_context_overflow() {
    fn tool(call: &str) -> Reply {
        Reply::http(
            200,
            "application/json",
            &json!({"id":format!("response-{call}"),"status":"completed","output":[{
                "type":"function_call","name":"shell","call_id":call,
                "arguments":json!({"command":format!("echo {call}")}).to_string()
            }]})
            .to_string(),
            "",
        )
    }
    let mut fixture = Fixture::with_history(
        2,
        10,
        5_000,
        true,
        Some(2_000),
        true,
        Some(80_000),
        "test resumed tool conversation",
    )
    .await;
    let first = fixture.incoming().await;
    assert!(!first.body.to_string().contains("DENSE_OLD_CONTEXT"));
    first.reply.send(tool("trim-first")).ok().unwrap();
    fixture
        .incoming_after_tool()
        .await
        .reply
        .send(Reply::error(429, ""))
        .ok()
        .unwrap();
    fixture
        .incoming()
        .await
        .reply
        .send(tool("trim-second"))
        .ok()
        .unwrap();
    fixture.incoming_after_tool().await.reply.send(Reply::http(400, "application/json", &json!({"error":{"code":"context_length_exceeded","message":"explicit later overflow"}}).to_string(), "")).ok().unwrap();
    let overflow_reduced = fixture.incoming().await;
    assert!(overflow_reduced.body["input"].as_array().unwrap().iter().any(|item| {
        item["type"] == "function_call_output" && item["call_id"] == "trim-first"
            && serde_json::from_str::<Value>(item["output"].as_str().unwrap()).unwrap()["omitted"] == true
    }));
    overflow_reduced.reply.send(Reply::success()).ok().unwrap();
    assert_eq!(fixture.settled().await, "completed");
    let attempts = fixture.attempts().await;
    assert_eq!(
        attempts
            .iter()
            .map(|(state, _)| state.as_str())
            .collect::<Vec<_>>(),
        ["completed", "failed", "completed", "failed", "completed"]
    );
    assert_eq!(
        attempts[1].1.as_ref().unwrap()["automatic_retry"]["number"],
        1
    );
    assert_eq!(
        attempts[3].1.as_ref().unwrap()["error"]["provider_code"],
        "context_length_exceeded"
    );
    fixture
        .control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM replacement WHERE format_version='context-trim-v1'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM replacement WHERE format_version='spill-v1'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                2,
                "already executed tools must not run again"
            );
            Ok(())
        })
        .await
        .unwrap();
    fixture.no_more_requests().await;
    fixture.close().await;
}
