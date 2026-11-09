//! `shellboy thread request` (#137) against a real owner and a loopback model:
//! rebuilt attempts are byte-identical to what the mock received, and two
//! consecutive requests in one tool loop share the whole earlier conversation.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shellboy::{
    app::DataOwner,
    config::{Config, PathEnvironment},
    control::Control,
    protocol::*,
    request_inspect::{RequestOptions, inspect},
};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

pub(super) struct Incoming {
    pub(super) raw: Vec<u8>,
    pub(super) reply: oneshot::Sender<Value>,
}
async fn proxy() -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    proxy_mode(false).await
}
pub(super) async fn proxy_mode(
    sse: bool,
) -> (
    String,
    mpsc::Receiver<Incoming>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut chunk = [0; 8192];
                let raw = loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..i]);
                        let length = head
                            .lines()
                            .find_map(|s| {
                                s.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap();
                        if bytes.len() >= i + 4 + length {
                            break bytes[i + 4..i + 4 + length].to_vec();
                        }
                    }
                };
                let (reply, result) = oneshot::channel();
                if send.send(Incoming { raw, reply }).await.is_err() {
                    return;
                }
                let Ok(body) = result.await else { return };
                let (kind, body) = if sse {
                    let event = if body["status"] == "incomplete" {
                        "response.incomplete"
                    } else {
                        "response.completed"
                    };
                    (
                        "text/event-stream",
                        format!(
                            "event: {event}\ndata: {}\n\n",
                            json!({"type":event,"response":body})
                        ),
                    )
                } else {
                    ("application/json", body.to_string())
                };
                let _ = stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
        }
    });
    (format!("http://{address}/v1"), receive, task)
}
pub(super) async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
pub(super) async fn incoming(receive: &mut mpsc::Receiver<Incoming>) -> Incoming {
    crate::test_watchdog::watchdog("tests/request_inspect.rs: incoming", receive.recv())
        .await
        .expect("loopback request watchdog")
        .expect("proxy open")
}
pub(super) async fn settled(control: &Control, thread: &str) {
    let mut events = control.subscribe();
    crate::test_watchdog::watchdog("tests/request_inspect.rs: settled", async {
        loop {
            let target = thread.to_owned();
            if control.store.database.call(move |db| Ok(db.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM turn WHERE thread_id=?1 AND state IN ('preparing','running','waiting'))",
                [target], |row| row.get::<_, bool>(0),
            )?)).await.unwrap() {
                break;
            }
            // Any event (or a lag) is a reason to re-read the saved state.
            if let Err(tokio::sync::broadcast::error::RecvError::Closed) = events.recv().await {
                panic!("event stream closed");
            }
        }
    })
    .await
    .expect("Thread settles");
}
pub(super) fn tool(call: &str, command: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"function_call","call_id":call,"name":"shell","arguments":json!({"command":command}).to_string()}]})
}
pub(super) fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(super) fn options(
    thread: &str,
    attempt: Option<&str>,
    compare: Option<&str>,
    body: bool,
) -> RequestOptions {
    RequestOptions {
        thread_id: thread.into(),
        attempt_id: attempt.map(str::to_owned),
        compare_attempt_id: compare.map(str::to_owned),
        body,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuilt_attempts_match_sent_bytes_and_consecutive_requests_share_the_conversation() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\n[connections.fixture.extra_body]\nservice_tier=\"flex\"\n"),
    )
    .unwrap();
    let data = root.path().join("data");
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
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
    request(
        &control,
        Command::MessageSend(MessageSendArgs {
            resume_recovery: false,

            human: false,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "inspect me".into(),
        }),
    )
    .await;
    let mut sent = Vec::new();
    for reply in [
        tool("first", "echo one"),
        tool("second", "echo two"),
        json!({"id":"done","status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"done"}]}]}),
    ] {
        let next = incoming(&mut receive).await;
        assert!(
            serde_json::from_slice::<Value>(&next.raw)
                .unwrap()
                .get("prompt_cache_options")
                .is_none(),
            "diagnostics default off"
        );
        sent.push(next.raw);
        next.reply.send(reply).unwrap();
    }
    settled(&control, &thread).await;
    let target = thread.clone();
    let attempts: Vec<String> = control
        .store
        .database
        .call(move |db| {
            Ok(db
                .prepare("SELECT a.id FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 ORDER BY a.started_at,a.rowid")?
                .query_map([target], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    assert_eq!(attempts.len(), 3);

    let loaded = Config::load_with_overrides(
        Some(&config),
        Some(&data),
        root.path(),
        &PathEnvironment::current(),
    )
    .unwrap();
    // Each rebuilt attempt is the exact body the model endpoint received.
    let mut rebuilt = Vec::new();
    for (attempt, raw) in attempts.iter().zip(&sent) {
        let result = inspect(&loaded, &options(&thread, Some(attempt), None, false)).unwrap();
        assert_eq!(result["unknown"], json!([]), "{result}");
        assert_eq!(result["form"], "wire");
        assert_eq!(result["total"]["sha256"], sha256(raw), "{attempt}");
        assert_eq!(result["total"]["bytes"], raw.len());
        assert_eq!(result["saved"]["request_body_bytes_match"], true);
        assert!(result.get("body").is_none(), "body only on request");
        rebuilt.push(result);
    }
    // In one tool loop, each request is a strict append of the previous one:
    // the context line is saved history, not a per-request tail (#183).
    for pair in attempts.windows(2) {
        let result = inspect(
            &loaded,
            &options(&thread, Some(&pair[0]), Some(&pair[1]), true),
        )
        .unwrap();
        let items = result["items"].as_array().unwrap().len();
        let contexts = result["body"]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| {
                item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Shellboy context v1: "))
            })
            .count();
        assert_eq!(contexts, 1, "{result}");
        assert_eq!(result["compare"]["common_prefix_items"], items, "{result}");
        assert_eq!(result["compare"]["first_difference"], items);
        assert!(result["compare"]["items"].as_u64().unwrap() > items as u64);
        // Opaque configuration stays opaque in the printed body.
        assert!(result["body"]["service_tier"]["sha256"].is_string());
        assert!(!result.to_string().contains("flex"));
    }
    // Without --attempt: the next request of the latest Turn from the current path.
    let next = inspect(&loaded, &options(&thread, None, Some(&attempts[2]), false)).unwrap();
    assert_eq!(next["source"], "next");
    assert_eq!(next["unknown"], json!([]));
    let last_attempt_items = rebuilt[2]["items"].as_array().unwrap().len();
    assert_eq!(next["compare"]["common_prefix_items"], last_attempt_items);
    assert!(next["items"].as_array().unwrap().len() > last_attempt_items);

    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[path = "request_inspect/history_faults.rs"]
mod history_faults;
