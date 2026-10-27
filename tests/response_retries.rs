//! Automatic Responses retries through a real owner, using only synthetic loopback traffic.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, protocol::*};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{sleep, timeout},
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
        let root = tempfile::tempdir().unwrap();
        let (url, requests, server) = proxy().await;
        let config = root.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms={max_delay_ms}\nstream_idle_timeout_ms={idle_ms}\n"
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
        let accepted = serde_json::from_value(
            request(
                &control,
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some(thread),
                    text: "synthetic retry regression".into(),
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

    async fn incoming(&mut self) -> Incoming {
        self.incoming_with_timeout(5).await
    }

    async fn incoming_after_tool(&mut self) -> Incoming {
        self.incoming_with_timeout(if cfg!(windows) { 15 } else { 5 })
            .await
    }

    async fn incoming_with_timeout(&mut self, seconds: u64) -> Incoming {
        timeout(Duration::from_secs(seconds), self.requests.recv())
            .await
            .expect("expected HTTP request")
            .unwrap()
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

    async fn settled(&self) -> String {
        let turn = self.accepted.turn_id.clone().unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                let turn = turn.clone();
                let state: String = self
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
                if !matches!(state.as_str(), "preparing" | "running" | "waiting") {
                    return state;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("turn did not settle")
    }

    async fn waiting_for_retry(&self) {
        timeout(Duration::from_secs(5), async {
            loop {
                if self.attempts().await.iter().any(|(state, error)| {
                    state == "failed"
                        && error
                            .as_ref()
                            .is_some_and(|value| value.get("automatic_retry").is_some())
                }) {
                    return;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("automatic retry was not persisted");
    }

    async fn no_more_requests(&mut self) {
        assert!(
            timeout(Duration::from_millis(50), self.requests.recv())
                .await
                .is_err(),
            "unexpected HTTP replay"
        );
    }

    async fn close(self) {
        timeout(Duration::from_secs(5), self.control.shutdown())
            .await
            .expect("Control shutdown did not finish")
            .unwrap();
        timeout(Duration::from_secs(5), self.owner.shutdown())
            .await
            .expect("owner shutdown did not finish")
            .unwrap();
        self.server.abort();
        let _ = self.server.await;
    }
}

async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = timeout(
        Duration::from_secs(5),
        control.request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        )),
    )
    .await
    .expect("Control request did not finish");
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_http_errors_retry_consumed_error_bodies_with_a_bounded_retry_after() {
    for status in [429, 500, 502, 503, 504] {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        let first = fixture.incoming().await;
        let original = first.body;
        let retry_after = if status == 503 {
            "Retry-After: Fri, 01 Jan 2100 00:00:00 GMT\r\n"
        } else {
            "Retry-After: 3600\r\n"
        };
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
async fn retry_budget_survives_successful_tool_groups_and_context_overflow_spill() {
    let mut fixture = Fixture::new(2, 10, 5_000).await;
    let mut pending = fixture.incoming().await;
    for number in 1..=2 {
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
            "incomplete"
        ]
    );
    assert_eq!(
        attempts[0].1.as_ref().unwrap()["automatic_retry"]["number"],
        1
    );
    assert_eq!(
        attempts[2].1.as_ref().unwrap()["automatic_retry"]["number"],
        2
    );
    assert_eq!(
        attempts[5].1.as_ref().unwrap()["automatic_retry_exhausted"],
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
                2
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM tool_result", [], |row| row
                    .get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM replacement WHERE format_version='spill-v1'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1
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
async fn any_successful_response_byte_blocks_replay_including_comments_and_tool_fragments() {
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
        .chain([
            Reply::http(200, "text/event-stream", ":", ""),
            Reply { bytes: b"HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: 99999\r\nConnection: close\r\n\r\n{".to_vec(), hold_open: false },
        ]);
    for reply in replies {
        let mut fixture = Fixture::new(2, 10, 5_000).await;
        fixture.incoming().await.reply.send(reply).ok().unwrap();
        assert_eq!(fixture.settled().await, "recovery_wait");
        let attempts = fixture.attempts().await;
        assert_eq!(attempts.len(), 1);
        assert!(
            attempts[0]
                .1
                .as_ref()
                .unwrap()
                .get("automatic_retry")
                .is_none()
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
async fn issue8_stall_timeout_never_automatically_retries_even_without_received_body_bytes() {
    let mut fixture = Fixture::new(2, 10, 500).await;
    fixture
        .incoming()
        .await
        .reply
        .send(Reply::partial("", true))
        .ok()
        .unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 1);
    let error = attempts[0].1.as_ref().unwrap();
    assert!(error.to_string().contains("timeout"));
    assert!(error.get("automatic_retry").is_none());
    fixture.no_more_requests().await;
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue8_stalled_error_body_keeps_http_status_but_does_not_retry() {
    let mut fixture = Fixture::new(2, 10, 500).await;
    fixture.incoming().await.reply.send(Reply {
        bytes: b"HTTP/1.1 503 Fixture\r\nContent-Type: application/json\r\nContent-Length: 99999\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{".to_vec(),
        hold_open: true,
    }).ok().unwrap();
    assert_eq!(fixture.settled().await, "recovery_wait");
    let attempts = fixture.attempts().await;
    assert_eq!(attempts.len(), 1);
    let saved = attempts[0].1.as_ref().unwrap();
    assert_eq!(saved["error"]["kind"], "timeout");
    assert_eq!(saved["error"]["status"], 503);
    assert!(
        saved["error"]["message"]
            .as_str()
            .unwrap()
            .contains("connections.fixture.stream_idle_timeout_ms")
    );
    assert!(saved.get("automatic_retry").is_none());
    fixture.no_more_requests().await;
    fixture.close().await;
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
