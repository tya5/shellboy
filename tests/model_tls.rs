//! Synthetic self-signed HTTPS: no external service, OS trust changes, or runtime openssl.
use base64::{Engine, engine::general_purpose::STANDARD};
use shellboy::{
    config::Connection,
    model::{http::HttpModelClient, response_request},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::{Duration, timeout},
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use tokio_util::sync::CancellationToken;

// Public, disposable test identity only. This key must never be used by a service.
const CERT: &str = "MIIBxTCCAWugAwIBAgIUPotsRpjhuQruGWHiAeBw0EBQpMMwCgYIKoZIzj0EAwIwKTEnMCUGA1UEAwwec2hlbGxib3kgc3ludGhldGljIFRMUyBmaXh0dXJlMCAXDTI2MTAyNTIzMTY1OVoYDzIxMjYxMDAxMjMxNjU5WjApMScwJQYDVQQDDB5zaGVsbGJveSBzeW50aGV0aWMgVExTIGZpeHR1cmUwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARTjAXCUtm10vJO3MTwjC2ggBIsIAvD7ltEH0Sw5FpEZVzI/3Ms0YN6IW6OkQlV1QV0xb1UIp4cexrcmn7YL8Zeo28wbTAdBgNVHQ4EFgQUkzvZEdszGbgPR/umNlZuAQLou44wHwYDVR0jBBgwFoAUkzvZEdszGbgPR/umNlZuAQLou44wDwYDVR0TAQH/BAUwAwEB/zAaBgNVHREEEzARhwR/AAABgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSAAwRQIgJAuCWGyALa0dn9daXeA6HAlXqpgSoKNQYzNgfA0/dbgCIQCh6mIw+0KVyEIsaKW+NfBZLrgqGCBfIbUODvS7LDT0nA==";
const KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgKt0duhDJwJbw5919Du+iCx/Z89sCHX2D02dkDuXtcXihRANCAARTjAXCUtm10vJO3MTwjC2ggBIsIAvD7ltEH0Sw5FpEZVzI/3Ms0YN6IW6OkQlV1QV0xb1UIp4cexrcmn7YL8Ze";

struct Fixture {
    url: String,
    requests: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn start() -> Self {
        let certificate = CertificateDer::from(STANDARD.decode(CERT).unwrap());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(STANDARD.decode(KEY).unwrap()));
        let tls = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                // Certificate rejection is an expected handshake failure, not a failed fixture.
                let Ok(Ok(mut stream)) =
                    timeout(Duration::from_secs(5), acceptor.accept(stream)).await
                else {
                    continue;
                };
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let split = loop {
                    let count = stream.read(&mut buf).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buf[..count]);
                    if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        break at + 4;
                    }
                    assert!(bytes.len() <= 65536);
                };
                let head = std::str::from_utf8(&bytes[..split]).unwrap();
                let route = head.lines().next().unwrap().to_owned();
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                assert!(length <= 65536);
                while bytes.len() < split + length {
                    let count = stream.read(&mut buf).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buf[..count]);
                }
                let request: serde_json::Value = if length == 0 {
                    serde_json::Value::Null
                } else {
                    serde_json::from_slice(&bytes[split..split + length]).unwrap()
                };
                let body = match route.as_str() {
                    "POST /v1/responses HTTP/1.1" => {
                        assert_eq!(request["model"], "synthetic-tls");
                        r#"{"id":"tls-fixture","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"synthetic TLS response"}]}]}"#
                    }
                    "GET /v1/models HTTP/1.1" => {
                        assert_eq!(length, 0);
                        r#"{"data":[{"id":"synthetic-tls-model"}]}"#
                    }
                    "POST /v1/embeddings HTTP/1.1" => {
                        assert_eq!(request["model"], "synthetic-tls");
                        assert_eq!(request["input"], serde_json::json!(["synthetic embedding"]));
                        r#"{"data":[{"index":0,"embedding":[0.25,0.75]}]}"#
                    }
                    _ => panic!("unexpected TLS fixture request: {route}"),
                };
                observed.fetch_add(1, Ordering::SeqCst);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.unwrap();
                stream.write_all(body.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
    fn client(&self, verification: Option<bool>) -> HttpModelClient {
        let mut config = format!("base_url = {:?}\n", self.url);
        if let Some(value) = verification {
            config.push_str(&format!("ssl_verify = {value}\n"));
        }
        let connection: Connection = toml::from_str(&config).unwrap();
        assert_eq!(connection.ssl_verify, verification.unwrap_or(true));
        HttpModelClient::new(connection).unwrap()
    }
}

#[tokio::test]
async fn self_signed_tls_requires_explicit_connection_opt_out_and_never_changes_other_clients() {
    let fixture = Fixture::start().await;
    let omitted = fixture.client(None);
    let explicit = fixture.client(Some(true));
    let insecure = fixture.client(Some(false));
    for client in [&omitted, &explicit] {
        let failure = timeout(
            Duration::from_secs(5),
            client.response(
                response_request("synthetic-tls", vec![], None, false),
                CancellationToken::new(),
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(failure.error.kind, "transport");
    }
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    let result = timeout(
        Duration::from_secs(5),
        insecure.response(
            response_request("synthetic-tls", vec![], None, false),
            CancellationToken::new(),
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.text, "synthetic TLS response");
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
    // Verify both existing clients and a newly built connection after opt-out use.
    let fresh = fixture.client(None);
    for client in [&omitted, &explicit, &fresh] {
        let failure = timeout(
            Duration::from_secs(5),
            client.response(
                response_request("synthetic-tls", vec![], None, false),
                CancellationToken::new(),
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(failure.error.kind, "transport");
    }
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
    assert!(
        !fixture.task.is_finished(),
        "fixture must still be healthy after rejected handshakes"
    );
}

#[tokio::test]
async fn tls_opt_out_covers_model_catalog_and_embeddings_without_leaking_to_other_connections() {
    let fixture = Fixture::start().await;
    let input = ["synthetic embedding".into()];
    for verification in [None, Some(true)] {
        let client = fixture.client(verification);
        let failure = timeout(
            Duration::from_secs(5),
            client.models(CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(failure.kind, "transport");
        let failure = timeout(
            Duration::from_secs(5),
            client.embeddings("synthetic-tls", &input, Some(2), CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(failure.kind, "transport");
    }
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    let client = fixture.client(Some(false));
    let models = timeout(
        Duration::from_secs(5),
        client.models(CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(models, ["synthetic-tls-model"]);
    let embedding = timeout(
        Duration::from_secs(5),
        client.embeddings("synthetic-tls", &input, Some(2), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(embedding.vectors, [vec![0.25, 0.75]]);
    assert_eq!(embedding.dimension, 2);
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    let fresh = fixture.client(None);
    assert_eq!(
        timeout(
            Duration::from_secs(5),
            fresh.models(CancellationToken::new())
        )
        .await
        .unwrap()
        .unwrap_err()
        .kind,
        "transport"
    );
    assert_eq!(
        timeout(
            Duration::from_secs(5),
            fresh.embeddings("synthetic-tls", &input, Some(2), CancellationToken::new())
        )
        .await
        .unwrap()
        .unwrap_err()
        .kind,
        "transport"
    );
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    assert!(
        !fixture.task.is_finished(),
        "fixture must still be healthy after rejected handshakes"
    );
}
