use super::*;
use crate::{config::Connection, model::response_request};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

struct Server {
    connection: Connection,
    accepts: Arc<AtomicUsize>,
    headers: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Clone, Copy)]
enum SseTail {
    Complete,
    Stalled,
    Oversized,
}
async fn server(label: &'static str, close: bool) -> Server {
    server_wire(label, close, None).await
}
async fn server_wire(label: &'static str, close: bool, sse: Option<SseTail>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Default::default()
    };
    let accepts = Arc::new(AtomicUsize::new(0));
    let headers = Arc::new(Mutex::new(Vec::new()));
    let seen = accepts.clone();
    let heads = headers.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            let heads = heads.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                loop {
                    let mut head = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        head.push_str(&line);
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|n| n.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).await.unwrap();
                    let payload = if head.starts_with("GET /v1/models ") {
                        json!({"data":[{"id":label}]})
                    } else if head.starts_with("POST /v1/embeddings ") {
                        json!({"data":[{"index":0,"embedding":[1.0,0.0]}]})
                    } else {
                        json!({"id":"synthetic-response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":label}]}]})
                    };
                    let response_request = head.starts_with("POST /v1/responses ");
                    heads.lock().unwrap().push(head);
                    if let Some(tail) = sse.filter(|_| response_request) {
                        let mut event = String::new();
                        if matches!(tail, SseTail::Stalled) {
                            for (kind, delta) in [
                                (
                                    "response.reasoning_text.delta",
                                    "retained synthetic reasoning",
                                ),
                                ("response.output_text.delta", "retained streamed text"),
                            ] {
                                event.push_str(&format!(
                                    "data: {}\n\n",
                                    json!({"type":kind,"delta":delta})
                                ));
                            }
                        }
                        event.push_str(&format!(
                            "data: {}\n\n",
                            json!({"type":"response.completed","response":payload})
                        ));
                        let first = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n{}\r\n",
                            event.len(),
                            event
                        );
                        if reader.get_mut().write_all(first.as_bytes()).await.is_err() {
                            return;
                        }
                        // The HTTP body finishes after the valid SSE terminal event.
                        // A reader stopping at that event cannot reuse this connection.
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        match tail {
                            SseTail::Complete => {
                                let trailing = "data: ignored trailing content\n\n";
                                let end =
                                    format!("{:x}\r\n{trailing}\r\n0\r\n\r\n", trailing.len());
                                if reader.get_mut().write_all(end.as_bytes()).await.is_err() {
                                    return;
                                }
                            }
                            SseTail::Stalled => {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                return;
                            }
                            SseTail::Oversized => {
                                let padding = "x".repeat(128 * 1024);
                                let end = format!("{:x}\r\n{padding}\r\n0\r\n\r\n", padding.len());
                                let _ = reader.get_mut().write_all(end.as_bytes()).await;
                                return;
                            }
                        }
                        continue;
                    }
                    let payload = payload.to_string();
                    let wire = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n{payload}",
                        payload.len(),
                        if close { "close" } else { "keep-alive" }
                    );
                    if reader.get_mut().write_all(wire.as_bytes()).await.is_err() || close {
                        return;
                    }
                }
            });
        }
    });
    Server {
        connection,
        accepts,
        headers,
        task,
    }
}

#[tokio::test]
async fn repeated_registry_lookups_share_tcp_across_catalog_embedding_and_json_response() {
    let server = server("shared", false).await;
    let mut config = Config::default();
    config
        .connections
        .insert("shared".into(), server.connection.clone());
    let clients = HttpClients::new(&config);
    assert_eq!(clients.snapshot().entries.len(), 1);
    assert_eq!(
        clients
            .get("shared")
            .unwrap()
            .models(CancellationToken::new())
            .await
            .unwrap(),
        ["shared"]
    );
    let embedding = clients
        .get("shared")
        .unwrap()
        .embeddings(
            "synthetic",
            &["synthetic".into()],
            Some(2),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(embedding.dimension, 2);
    let request = response_request(
        "synthetic",
        vec![json!({"role":"user","content":"synthetic"})],
        None,
        false,
    );
    let (events, _receive) = tokio::sync::mpsc::channel(16);
    let response = clients
        .get("shared")
        .unwrap()
        .response(request, CancellationToken::new(), Some(events))
        .await
        .unwrap();
    assert_eq!(response.text, "shared");
    assert_eq!(server.headers.lock().unwrap().len(), 3);
    assert_eq!(
        server.accepts.load(Ordering::SeqCst),
        1,
        "fresh wrappers must share the reqwest pool"
    );
}

#[tokio::test]
async fn distinct_profiles_keep_their_endpoint_headers_tls_and_timeout_settings() {
    let first = server("first", false).await;
    let second = server("second", false).await;
    let mut a = first.connection.clone();
    a.ssl_verify = false;
    a.request_timeout_ms = 1234;
    a.headers.insert("X-Profile".into(), "first".into());
    let mut b = second.connection.clone();
    b.request_timeout_ms = 5678;
    b.headers.insert("X-Profile".into(), "second".into());
    let config = Config {
        connections: BTreeMap::from([("first".into(), a), ("second".into(), b)]),
        ..Default::default()
    };
    let clients = HttpClients::new(&config);
    for (name, ssl, deadline) in [("first", false, 1234), ("second", true, 5678)] {
        let client = clients.get(name).unwrap();
        assert_eq!(client.connection.ssl_verify, ssl);
        assert_eq!(client.connection.request_timeout_ms, deadline);
        assert_eq!(
            client.models(CancellationToken::new()).await.unwrap(),
            [name]
        );
        assert_eq!(
            clients
                .get(name)
                .unwrap()
                .models(CancellationToken::new())
                .await
                .unwrap(),
            [name]
        );
    }
    for (fixture, expected) in [(&first, "x-profile: first"), (&second, "x-profile: second")] {
        assert_eq!(fixture.accepts.load(Ordering::SeqCst), 1);
        assert!(
            fixture
                .headers
                .lock()
                .unwrap()
                .iter()
                .all(|head| head.to_ascii_lowercase().contains(expected))
        );
    }
    assert!(clients.get("missing").is_err());
}

#[tokio::test]
async fn server_closed_idle_connection_is_replaced_on_the_next_request() {
    let server = server("closed", true).await;
    let config = Config {
        connections: BTreeMap::from([("closed".into(), server.connection.clone())]),
        ..Default::default()
    };
    let clients = HttpClients::new(&config);
    for _ in 0..2 {
        assert_eq!(
            clients
                .get("closed")
                .unwrap()
                .models(CancellationToken::new())
                .await
                .unwrap(),
            ["closed"]
        );
    }
    assert_eq!(server.accepts.load(Ordering::SeqCst), 2);
}

#[test]
fn credential_rotation_runs_in_an_isolated_process() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "model::http::clients::tests::credential_rotation_child",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("SHELLBOY_TEST_CREDENTIAL_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "isolated child of credential_rotation_runs_in_an_isolated_process"]
fn credential_rotation_child() {
    assert_eq!(
        std::env::var("SHELLBOY_TEST_CREDENTIAL_CHILD").as_deref(),
        Ok("1")
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(server("credentials", false));
    let mut connection = server.connection.clone();
    connection.api_key_env = Some("SHELLBOY_SYNTHETIC_ROTATING_KEY".into());
    let clients = HttpClients::new(&Config {
        connections: BTreeMap::from([("credentials".into(), connection)]),
        ..Default::default()
    });
    for key in ["synthetic-first", "synthetic-second"] {
        // This sealed subprocess runs exactly one test and a current-thread
        // runtime. No executor tasks run while its synthetic env is changed.
        unsafe {
            std::env::set_var("SHELLBOY_SYNTHETIC_ROTATING_KEY", key);
        }
        runtime
            .block_on(
                clients
                    .get("credentials")
                    .unwrap()
                    .models(CancellationToken::new()),
            )
            .unwrap();
    }
    let heads = server.headers.lock().unwrap();
    assert!(
        heads[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-first")
    );
    assert!(
        heads[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-second")
    );
    assert_eq!(server.accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn initialization_failure_is_local_to_one_configured_profile() {
    let server = server("valid", false).await;
    let config = Config {
        connections: BTreeMap::from([
            ("valid".into(), server.connection.clone()),
            (
                "invalid".into(),
                Connection {
                    base_url: "not-a-url".into(),
                    ..Default::default()
                },
            ),
        ]),
        ..Default::default()
    };
    let clients = HttpClients::new(&config);
    assert_eq!(clients.snapshot().entries.len(), 2);
    assert!(clients.get("invalid").is_err());
    assert_eq!(
        clients
            .get("valid")
            .unwrap()
            .models(CancellationToken::new())
            .await
            .unwrap(),
        ["valid"]
    );
}

#[tokio::test]
async fn control_catalog_requests_reuse_the_owner_client_pool() {
    use crate::{
        app::DataOwner,
        control::Control,
        protocol::{Command, ModelListArgs, Request},
    };
    let fixture = server("catalog", false).await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(
        &path,
        format!(
            "default_connection='catalog'\n[connections.catalog]\nbase_url={:?}\n",
            fixture.connection.base_url
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
    for i in 0..2 {
        let result = control
            .request(Request::new(
                format!("{}:catalog-{i}", owner.instance_id),
                Command::ModelList(ModelListArgs {
                    connection: Some("catalog".into()),
                    thread_id: None,
                }),
            ))
            .await;
        assert!(result.ok, "{:?}", result.error);
    }
    assert_eq!(
        fixture.accepts.load(Ordering::SeqCst),
        1,
        "Control must not build a client per catalog request"
    );
    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "opt-in: exactly four synthetic requests to local LiteLLM; may incur API charges"]
async fn local_proxy_four_request_pool_timing_probe() {
    let model =
        std::env::var("SHELLBOY_TEST_PROXY_MODEL").expect("explicit synthetic model required");
    let connection = Connection {
        base_url: "http://127.0.0.1:4000/v1".into(),
        request_timeout_ms: 45_000,
        stream_idle_timeout_ms: 45_000,
        api_key_env: std::env::var_os("SHELLBOY_TEST_PROXY_API_KEY")
            .map(|_| "SHELLBOY_TEST_PROXY_API_KEY".into()),
        ..Default::default()
    };
    let clients = HttpClients::new(&Config {
        connections: BTreeMap::from([("probe".into(), connection.clone())]),
        ..Default::default()
    });
    for shared in [false, true] {
        for sample in 0..2 {
            let client = if shared {
                clients.get("probe").unwrap()
            } else {
                HttpModelClient::for_connection("probe", connection.clone()).unwrap()
            };
            let mut request = response_request(
                &model,
                vec![json!({"role":"user","content":"Synthetic connection test. Reply only OK."})],
                None,
                false,
            );
            request["max_output_tokens"] = json!(512);
            let started = std::time::Instant::now();
            crate::test_watchdog::watchdog(
                "src/model/http/clients/tests.rs: local_proxy_four_request_pool_timing_probe",
                client.response(request, CancellationToken::new(), None),
            )
            .await
            .expect("probe deadline")
            .expect("probe response");
            eprintln!(
                "pool_probe shared={shared} sample={sample} elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
    }
}

#[tokio::test]
async fn sse_terminal_drain_reuses_one_tcp_without_parsing_trailing_content() {
    let fixture = server_wire("sse", false, Some(SseTail::Complete)).await;
    let client = HttpModelClient::new(fixture.connection.clone()).unwrap();
    for _ in 0..2 {
        let response = client
            .response_for_thread(
                "synthetic-thread",
                response_request("synthetic", vec![], None, false),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.text, "sse");
    }
    assert_eq!(fixture.headers.lock().unwrap().len(), 2);
    assert_eq!(
        fixture.accepts.load(Ordering::SeqCst),
        1,
        "SSE responses must return the drained HTTP1 connection to the pool"
    );
}

#[tokio::test]
async fn sse_terminal_drain_is_bounded_for_stalled_or_oversized_tail() {
    for tail in [SseTail::Stalled, SseTail::Oversized] {
        let fixture = server_wire("already completed", false, Some(tail)).await;
        let client = HttpModelClient::new(fixture.connection.clone()).unwrap();
        let response = crate::test_watchdog::watchdog("src/model/http/clients/tests.rs: sse_terminal_drain_is_bounded_for_stalled_or_oversized_tail",
            client.response(
                response_request("synthetic", vec![], None, false),
                CancellationToken::new(),
                None,
            ),
        )
        .await
        .expect("terminal cleanup must be bounded")
        .unwrap();
        assert_eq!(response.text, "already completed");
    }
}

#[tokio::test]
async fn control_conversations_reuse_owner_client_for_chunked_sse() {
    use crate::{
        app::DataOwner,
        control::Control,
        protocol::{Command, MessageSendArgs, Request, ThreadCreateArgs},
    };
    let fixture = server_wire("sse conversation", false, Some(SseTail::Complete)).await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path, format!("default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url={:?}\n", fixture.connection.base_url)).unwrap();
    let owner = DataOwner::start(
        Some(path),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let create = control
        .request(Request::new(
            format!("{}:create", owner.instance_id),
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                cwd: None,
                parent_id: None,
                connection: None,
                model: None,
                effort: None,
            }),
        ))
        .await;
    assert!(create.ok, "{:?}", create.error);
    let thread = create.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for n in 0..2 {
        let sent = control
            .request(Request::new(
                format!("{}:send-{n}", owner.instance_id),
                Command::MessageSend(MessageSendArgs {
                    human: false,
                    to_human: false,
                    expect_reply: false,
                    sender_label: None,
                    deliver: crate::protocol::Deliver::Next,
                    thread_id: Some(thread.clone()),
                    text: format!("synthetic {n}"),
                }),
            ))
            .await;
        assert!(sent.ok, "{:?}", sent.error);
        let turn = sent.result.unwrap()["turn_id"].as_str().unwrap().to_owned();
        crate::test_watchdog::watchdog("src/model/http/clients/tests.rs: control_conversations_reuse_owner_client_for_chunked_sse", async {
            loop {
                let target = turn.clone();
                let state = control
                    .store
                    .database
                    .call(move |db| {
                        Ok(
                            db.query_row("SELECT state FROM turn WHERE id=?1", [target], |row| {
                                row.get::<_, String>(0)
                            })?,
                        )
                    })
                    .await
                    .unwrap();
                if state == "completed" {
                    break;
                }
                assert_ne!(state, "recovery_wait");
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(fixture.headers.lock().unwrap().len(), 2);
    assert_eq!(
        fixture.accepts.load(Ordering::SeqCst),
        1,
        "conversation driver must use owner-scoped pooled client"
    );
    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "opt-in: four synthetic prompt-cache requests to local LiteLLM; may incur API charges"]
async fn local_proxy_thread_session_cache_probe() {
    let model =
        std::env::var("SHELLBOY_TEST_PROXY_MODEL").expect("explicit synthetic model required");
    let marker = uuid::Uuid::new_v4().to_string();
    let prefix = (0..1800)
        .map(|n| format!("Synthetic record {n:04}: amber cedar river meadow stone.\n"))
        .collect::<String>();
    let input = format!(
        "Synthetic cache probe {marker}. Read the following synthetic records and reply only OK.\n{prefix}"
    );
    let session = uuid::Uuid::new_v4().to_string();
    for enabled in [false, true] {
        let connection = Connection {
            base_url: "http://127.0.0.1:4000/v1".into(),
            stream_idle_timeout_ms: 90_000,
            session_header: enabled,
            api_key_env: std::env::var_os("SHELLBOY_TEST_PROXY_API_KEY")
                .map(|_| "SHELLBOY_TEST_PROXY_API_KEY".into()),
            ..Default::default()
        };
        let client = HttpModelClient::new(connection).unwrap();
        for sample in 0..2 {
            let mut request = response_request(
                &model,
                vec![json!({"role":"user","content":input})],
                Some("low"),
                false,
            );
            request["max_output_tokens"] = json!(512);
            let started = std::time::Instant::now();
            let response = crate::test_watchdog::watchdog(
                "src/model/http/clients/tests.rs: local_proxy_thread_session_cache_probe",
                client.response_for_thread(&session, request, CancellationToken::new(), None),
            )
            .await
            .expect("probe deadline")
            .expect("probe response");
            eprintln!(
                "session_cache_probe header={enabled} sample={sample} elapsed_ms={} usage={}",
                started.elapsed().as_millis(),
                response
                    .raw
                    .get("usage")
                    .unwrap_or(&serde_json::Value::Null)
            );
        }
    }
}

#[tokio::test]
async fn sse_terminal_drain_cancellation_is_prompt_and_preserves_final_raw() {
    let fixture = server_wire(
        "completed before cancellation",
        false,
        Some(SseTail::Stalled),
    )
    .await;
    let client = HttpModelClient::new(fixture.connection.clone()).unwrap();
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let (events, mut receive) = tokio::sync::mpsc::channel(4);
    let task = tokio::spawn(async move {
        client
            .response(
                response_request("synthetic", vec![], None, false),
                stopped,
                Some(events),
            )
            .await
    });
    crate::test_watchdog::watchdog("src/model/http/clients/tests.rs: sse_terminal_drain_cancellation_is_prompt_and_preserves_final_raw", async {
        while let Some(event) = receive.recv().await {
            if event["type"] == "response.completed" {
                return;
            }
        }
        panic!("terminal event was not delivered");
    })
    .await
    .unwrap();
    cancel.cancel();
    let failure = crate::test_watchdog::watchdog("src/model/http/clients/tests.rs: sse_terminal_drain_cancellation_is_prompt_and_preserves_final_raw", task)
        .await
        .expect("drain must observe cancellation")
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.error.kind, "cancelled");
    assert_eq!(failure.partial_reasoning, "retained synthetic reasoning");
    assert_eq!(failure.partial_text, "retained streamed text");
    assert_eq!(failure.raw_response.unwrap()["status"], "completed");
}

#[tokio::test(start_paused = true)]
async fn usage_limit_slots_are_connection_bounded_shared_and_expire_on_tokio_clock() {
    let mut config = Config::default();
    config.connections.insert(
        "one".into(),
        Connection {
            base_url: "http://127.0.0.1:1/v1".into(),
            ..Default::default()
        },
    );
    config.connections.insert(
        "two".into(),
        Connection {
            base_url: "http://127.0.0.1:1/v1".into(),
            ..Default::default()
        },
    );
    let clients = HttpClients::new(&config);
    assert_eq!(clients.snapshot().entries.len(), 2);
    let one = clients.get("one").unwrap();
    let fact = crate::model::usage_limit::UsageLimit::parse(
        &json!({"error":{
        "type":"usage_limit_reached","resets_in_seconds":3600}}),
        chrono::Utc::now(),
    )
    .unwrap();
    one.remember_usage_limit(fact);
    let mut earlier = fact;
    earlier.resets_at -= 3000;
    one.remember_usage_limit(earlier);
    assert_eq!(one.active_usage_limit().unwrap().resets_at, fact.resets_at);
    assert!(clients.get("unconfigured").is_err());
    assert!(
        clients
            .get("one")
            .unwrap()
            .active_usage_limit()
            .unwrap()
            .locally_blocked
    );
    assert!(clients.get("two").unwrap().active_usage_limit().is_none());
    assert!(
        HttpClients::new(&config)
            .get("one")
            .unwrap()
            .active_usage_limit()
            .is_none()
    );
    tokio::time::advance(std::time::Duration::from_secs(3599)).await;
    assert!(one.active_usage_limit().is_some());
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    assert!(one.active_usage_limit().is_none());

    one.remember_usage_limit(fact);
    let mut newer = fact;
    newer.resets_at += 60;
    one.remember_usage_limit(newer);
    one.clear_usage_limit_if(fact);
    assert_eq!(one.active_usage_limit().unwrap().resets_at, newer.resets_at);
    one.clear_usage_limit_if(newer);
    assert!(one.active_usage_limit().is_none());
}

#[tokio::test]
async fn usage_limit_http_error_is_shared_by_responses_and_chat_completions() {
    for api in [
        crate::config::Api::Responses,
        crate::config::Api::ChatCompletions,
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection = Connection {
            api,
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            ..Default::default()
        };
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut head = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(head.starts_with(if api == crate::config::Api::Responses {
                "POST /v1/responses "
            } else {
                "POST /v1/chat/completions "
            }));
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse().unwrap())
                })
                .unwrap();
            reader.read_exact(&mut vec![0; length]).await.unwrap();
            let body = json!({"error":{"type":"usage_limit_reached","resets_in_seconds":3600}})
                .to_string();
            reader.get_mut().write_all(format!("HTTP/1.1 429 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let client = HttpModelClient::new(connection).unwrap();
        let failure = crate::test_watchdog::watchdog(
            "quota dialect HTTP fact",
            client.response(
                response_request("synthetic", vec![], None, false),
                CancellationToken::new(),
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(failure.facts.status, Some(429));
        assert!(failure.error.usage_limit.is_some());
        task.await.unwrap();
        let failure = client
            .response(
                response_request("synthetic", vec![], None, false),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        assert!(failure.error.usage_limit.unwrap().locally_blocked);
        assert!(failure.facts.http_observation.is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn connection_wait_state_is_shared_by_clones_but_not_profiles_or_restart() {
    let mut config = Config::default();
    config.connections.clear();
    let connection = Connection {
        base_url: "http://127.0.0.1:12345/v1".into(),
        ..Default::default()
    };
    config.connections.insert("one".into(), connection.clone());
    config.connections.insert("two".into(), connection);
    let clients = HttpClients::new(&config);
    let first = clients.get("one").unwrap();
    let same = clients.get("one").unwrap();
    let different = clients.get("two").unwrap();
    first.rate_gate.throttle(
        Some(std::time::Duration::from_secs(5)),
        first.rate_gate.generation(),
    );
    let token = CancellationToken::new();
    assert!(different.rate_gate.acquire(&token).await.is_ok());
    let waiting = tokio::spawn({
        let token = token.clone();
        async move { same.rate_gate.acquire(&token).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    token.cancel();
    assert!(waiting.await.unwrap().is_err());
    let restarted = HttpClients::new(&config).get("one").unwrap();
    assert!(
        restarted
            .rate_gate
            .acquire(&CancellationToken::new())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn real_429_headers_update_shared_profile_before_body_finishes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Default::default()
    };
    let client = HttpModelClient::for_connection("fixture", connection).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut body = [0u8; 8192];
        let _ = stream.read(&mut body).await.unwrap();
        stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\nContent-Length: 9999\r\n\r\n").await.unwrap();
        std::future::pending::<()>().await;
    });
    let request_cancel = CancellationToken::new();
    let request = tokio::spawn({
        let client = client.clone();
        let token = request_cancel.clone();
        async move { client.models(token).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while client.rate_gate.generation() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("429 header not observed");
    assert!(
        !request.is_finished(),
        "fixture response body must remain open"
    );
    let cancel = CancellationToken::new();
    let waiting = tokio::spawn({
        let gate = client.rate_gate.clone();
        let cancel = cancel.clone();
        async move { gate.acquire(&cancel).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    cancel.cancel();
    assert!(waiting.await.unwrap().is_err());
    request_cancel.cancel();
    let _ = request.await;
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn catalog_waits_for_profile_and_cancels_before_sending() {
    let server = server("catalog", false).await;
    let client = HttpModelClient::for_connection("fixture", server.connection.clone()).unwrap();
    client.rate_gate.throttle(
        Some(std::time::Duration::from_secs(30)),
        client.rate_gate.generation(),
    );
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let client = client.clone();
        let cancel = cancel.clone();
        async move { client.models(cancel).await }
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert_eq!(server.accepts.load(Ordering::SeqCst), 0);
    cancel.cancel();
    assert_eq!(task.await.unwrap().unwrap_err().kind, "cancelled");
    assert_eq!(server.accepts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn catalog_wait_is_shared_by_profile_but_isolated_from_other_profiles() {
    let server = server("catalog", false).await;
    let config = Config {
        connections: BTreeMap::from([
            ("one".into(), server.connection.clone()),
            ("two".into(), server.connection.clone()),
        ]),
        ..Default::default()
    };
    let clients = HttpClients::new(&config);
    let first = clients.get("one").unwrap();
    let same = clients.get("one").unwrap();
    let other = clients.get("two").unwrap();
    first.rate_gate.throttle(
        Some(std::time::Duration::from_secs(30)),
        first.rate_gate.generation(),
    );
    let cancel = CancellationToken::new();
    let waiting = tokio::spawn({
        let cancel = cancel.clone();
        async move { same.models(cancel).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        other.models(CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, ["catalog"]);
    assert!(!waiting.is_finished());
    cancel.cancel();
    assert_eq!(waiting.await.unwrap().unwrap_err().kind, "cancelled");
    assert_eq!(server.headers.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn success_headers_release_the_probe_while_the_body_is_still_streaming() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Default::default()
    };
    let client = HttpModelClient::for_connection("fixture", connection).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut body = [0u8; 8192];
        let _ = stream.read(&mut body).await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 9999\r\n\r\n{").await.unwrap();
        std::future::pending::<()>().await;
    });
    client
        .rate_gate
        .throttle(None, client.rate_gate.generation());
    let cancel = CancellationToken::new();
    let probe = tokio::spawn({
        let client = client.clone();
        let cancel = cancel.clone();
        async move { client.models(cancel).await }
    });
    // Ready means the cooldown cleared and the probe permit was released even
    // though the probe response body never finishes.
    crate::test_watchdog::watchdog("probe permit released at success headers", async {
        loop {
            if matches!(
                client.reserve_rate_admission(None),
                Ok(super::super::RateReservation::Ready(_))
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("probe permit was not released at success headers");
    assert!(!probe.is_finished(), "probe body must still be streaming");
    cancel.cancel();
    let _ = probe.await;
    server.abort();
}

#[tokio::test]
async fn queued_waiter_deferred_by_usage_limit_reports_the_limit() {
    let connection = Connection {
        base_url: "http://127.0.0.1:1/v1".into(),
        ..Default::default()
    };
    let client = HttpModelClient::for_connection("fixture", connection).unwrap();
    client.rate_gate.throttle(
        Some(std::time::Duration::from_secs(30)),
        client.rate_gate.generation(),
    );
    let Ok(super::super::RateReservation::Waiting(waiter)) = client.reserve_rate_admission(None)
    else {
        panic!("expected a queued waiter")
    };
    let queued = tokio::spawn(async move { waiter.wait(&CancellationToken::new()).await });
    tokio::task::yield_now().await;
    // The usage-limit 429 arrives while the request is queued.
    let fact = crate::model::usage_limit::UsageLimit::parse(
        &json!({"error":{"type":"usage_limit_reached","resets_in_seconds":3600}}),
        chrono::Utc::now(),
    )
    .unwrap();
    client.remember_usage_limit(fact);
    client.rate_gate.throttle(
        Some(std::time::Duration::from_secs(3600)),
        client.rate_gate.generation(),
    );
    let result = queued.await.unwrap();
    assert_eq!(result.as_ref().err().unwrap().kind, "rate_limit_deferred");
    match client.admission_after_wait(result, None).unwrap() {
        super::super::RateAdmission::QuotaBlocked(blocked) => {
            assert_eq!(blocked.resets_at, fact.resets_at)
        }
        super::super::RateAdmission::Lease(_) => panic!("deferred waiter must not hold a lease"),
    }
    // Without an active usage limit the generic deferral is preserved.
    let plain = HttpModelClient::for_connection(
        "plain",
        Connection {
            base_url: "http://127.0.0.1:1/v1".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let error = ModelError::new("rate_limit_deferred", "x");
    assert!(plain.admission_after_wait(Err(error), None).is_err());
}

#[tokio::test]
async fn catalog_429_long_retry_after_defers_next_call_without_a_send() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Default::default()
    };
    let client = HttpModelClient::for_connection("fixture", connection).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 8192];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        let body = r#"{"error":{"message":"synthetic"}}"#;
        stream.write_all(format!("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 601\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
        listener
    });
    assert!(client.models(CancellationToken::new()).await.is_err());
    let listener = server.await.unwrap();
    let error = client.models(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind, "rate_limit_deferred");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn error_completion_fact_distinguishes_eof_from_interrupted_body() {
    for complete in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection = Connection {
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            ..Default::default()
        };
        let client = HttpModelClient::for_connection("fixture", connection).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            let body = r#"{"error":{"message":"synthetic"}}"#;
            let size = if complete {
                body.len()
            } else {
                body.len() + 100
            };
            stream.write_all(format!("HTTP/1.1 500 Error\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await.unwrap();
        });
        let failure = client
            .response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(failure.facts.status, Some(500));
        assert_eq!(failure.facts.http_error_complete, complete);
    }
}

#[tokio::test]
async fn catalog_429_blocks_embedding_on_same_profile_until_cancelled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Default::default()
    };
    let client = HttpModelClient::for_connection("fixture", connection).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 8192];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        let body = r#"{"error":{"message":"synthetic"}}"#;
        stream.write_all(format!("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
        listener
    });
    assert!(client.models(CancellationToken::new()).await.is_err());
    let listener = server.await.unwrap();
    let cancel = CancellationToken::new();
    let embedding = tokio::spawn({
        let client = client.clone();
        let token = cancel.clone();
        async move {
            client
                .embeddings("fixture", &["x".to_owned()], None, None, token)
                .await
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
    assert!(!embedding.is_finished());
    cancel.cancel();
    assert_eq!(embedding.await.unwrap().unwrap_err().kind, "cancelled");
}

#[tokio::test]
async fn live_connections_rebuild_only_changed_clients_and_keep_existing_generations() {
    let mut config = Config::parse("default_connection='chat'\ndefault_model='synthetic'\n[connections.chat]\nbase_url='http://127.0.0.1:1/v1'\n[connections.keep]\nbase_url='http://127.0.0.1:2/v1'\n").unwrap();
    let clients = config.live_connections.clients(&config);
    let old = clients.snapshot();
    let old_chat = old.get("chat").unwrap();
    let keep = old.get("keep").unwrap();
    config.connections.get_mut("chat").unwrap().base_url = "http://127.0.0.1:3/v1".into();
    config.default_model = Some("new-model".into());
    clients.adopt(&config, None);
    let next = clients.snapshot();
    assert!(Arc::ptr_eq(&keep.pool, &next.get("keep").unwrap().pool));
    assert!(!Arc::ptr_eq(
        &old_chat.pool,
        &next.get("chat").unwrap().pool
    ));
    assert_eq!(old_chat.connection.base_url, "http://127.0.0.1:1/v1");
    assert_eq!(
        next.get("chat").unwrap().connection.base_url,
        "http://127.0.0.1:3/v1"
    );
    assert_eq!(old.default_model.as_deref(), Some("synthetic"));
    assert_eq!(next.default_model.as_deref(), Some("new-model"));
    assert_eq!(config.current().default_model.as_deref(), Some("new-model"));
    config.connections.remove("chat");
    config.default_connection = Some("keep".into());
    clients.adopt(&config, None);
    assert!(clients.get("chat").is_err());
    assert!(old.get("chat").is_ok());
    assert_eq!(config.current().default_connection.as_deref(), Some("keep"));
}

#[tokio::test(start_paused = true)]
async fn live_connections_admission_wait_preserves_lease_endpoint_and_wire_generation() {
    use super::super::{RateAdmission, RateReservation};
    let mut config = Config::parse("[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\ncontext_role='developer'\n[connections.fixture.extra_body]\nsynthetic_generation='old'\n").unwrap();
    let owner = HttpClients::new(&config);
    let run = owner.snapshot();
    let client = run.get("fixture").unwrap();
    client.rate_gate.throttle(
        Some(std::time::Duration::from_secs(30)),
        client.rate_gate.generation(),
    );
    let RateReservation::Waiting(waiter) = client.reserve_rate_admission(None).unwrap() else {
        panic!("cooldown must queue admission")
    };
    config.connections.get_mut("fixture").unwrap().base_url = "http://127.0.0.1:2/v1".into();
    config
        .connections
        .get_mut("fixture")
        .unwrap()
        .extra_body
        .insert("synthetic_generation".into(), serde_json::json!("new"));
    owner.adopt(&config, None);
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    let admitted = client
        .admission_after_wait(waiter.wait(&CancellationToken::new()).await, None)
        .unwrap();
    assert!(matches!(admitted, RateAdmission::Lease(_)));
    let selected = run.get("fixture").unwrap();
    assert!(Arc::ptr_eq(&selected.pool, &client.pool));
    assert_eq!(selected.connection.base_url, "http://127.0.0.1:1/v1");
    let body = serde_json::json!({"model":"synthetic","input":[{"role":"developer","content":[{"type":"input_text","text":"synthetic"}]}],"tools":[]});
    assert_eq!(
        selected.wire_request(body.clone(), None).unwrap().0["synthetic_generation"],
        "old"
    );
    assert_eq!(
        owner
            .get("fixture")
            .unwrap()
            .wire_request(body, None)
            .unwrap()
            .0["synthetic_generation"],
        "new"
    );
    drop(admitted);
}
