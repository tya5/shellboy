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
async fn server(label: &'static str, close: bool) -> Server {
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
                    heads.lock().unwrap().push(head);
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
    assert_eq!(clients.entries.len(), 1);
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
    assert_eq!(clients.entries.len(), 2);
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
            tokio::time::timeout(
                std::time::Duration::from_secs(45),
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
