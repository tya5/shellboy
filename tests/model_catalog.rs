use serde_json::json;
use shellboy::{
    config::{ApiEndpoint, Connection},
    model::http::HttpModelClient,
};
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;

async fn request_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 8192);
    }
    String::from_utf8(bytes).unwrap()
}
fn connection(base_url: String) -> Connection {
    Connection {
        base_url,
        headers: BTreeMap::from([
            ("Authorization".into(), "Bearer catalog-private-key".into()),
            ("X-Proxy-Token".into(), "catalog-private-header".into()),
        ]),
        ..Connection::default()
    }
}
async fn fixture(
    status: u16,
    body: Vec<u8>,
) -> (
    HttpModelClient,
    oneshot::Receiver<String>,
    count_after::CountAfter,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = HttpModelClient::new(connection(format!(
        "http://{}/v1/",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let (tx, rx) = oneshot::channel();
    let (finished, completion) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = tx.send(request_head(&mut stream).await);
        let head = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if stream.write_all(head.as_bytes()).await.is_ok() {
            let _ = stream.write_all(&body).await;
        }
        drop(stream);
        let _ = completion.await;
        if futures_util::FutureExt::now_or_never(listener.accept()).is_some() {
            2
        } else {
            1
        }
    });
    (client, rx, count_after::CountAfter::new(server, finished))
}
#[test]
fn models_endpoint_preserves_configured_api_root() {
    for (root, expected) in [
        ("http://localhost:4000", "/models"),
        ("http://localhost:4000/v1/", "/v1/models"),
        ("http://localhost:4000/proxy/v1", "/proxy/v1/models"),
    ] {
        assert_eq!(
            connection(root.into())
                .endpoint(ApiEndpoint::Models)
                .unwrap()
                .path(),
            expected
        );
    }
}
#[tokio::test]
async fn catalog_get_preserves_auth_headers_and_sorts_deduplicates_without_capability_filtering() {
    let (client, request, server) = fixture(200, serde_json::to_vec(&json!({"object":"list", "data":[
        {"id":"z-chat"}, {"id":"a-embedding", "capabilities":["embeddings"]}, {"id":"z-chat"}, {"id":"日本語-model"}
    ]})).unwrap()).await;
    assert_eq!(
        client.models(CancellationToken::new()).await.unwrap(),
        ["a-embedding", "z-chat", "日本語-model"]
    );
    let request = request.await.unwrap().to_ascii_lowercase();
    assert!(request.starts_with("get /v1/models http/1.1\r\n"));
    assert!(request.contains("authorization: bearer catalog-private-key\r\n"));
    assert!(request.contains("x-proxy-token: catalog-private-header\r\n"));
    assert!(request.contains("accept: application/json\r\n"));
    assert!(!request.contains("content-type:"));
    assert!(!request.contains("content-length:"));
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn catalog_reports_http_status_and_redacts_provider_diagnostics_without_retry() {
    for status in [401, 403, 404, 429] {
        let body = json!({"error":{"code":"catalog-private-header", "message":"Bearer catalog-private-key catalog-private-header"}});
        let (client, _, server) = fixture(status, serde_json::to_vec(&body).unwrap()).await;
        let error = client.models(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.status, Some(status));
        assert!(!error.message.contains("catalog-private"));
        assert_eq!(error.provider_code.as_deref(), Some("[redacted]"));
        assert_eq!(server.await.unwrap(), 1);
    }
}
#[tokio::test]
async fn catalog_never_follows_redirect() {
    let (client, _, server) = fixture(302, b"redirect".to_vec()).await;
    assert_eq!(
        client
            .models(CancellationToken::new())
            .await
            .unwrap_err()
            .status,
        Some(302)
    );
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn catalog_rejects_provider_error_even_with_success_status() {
    let (client, _, server) = fixture(
        200,
        serde_json::to_vec(&json!({"error":{"message":"catalog-private-key rejected"}})).unwrap(),
    )
    .await;
    let error = client.models(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind, "provider");
    assert_eq!(error.status, Some(200));
    assert!(!error.message.contains("catalog-private-key"));
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn catalog_rejects_malformed_json_schema_and_invalid_ids_without_echoing_them() {
    let mut bodies = vec![b"not-json".to_vec()];
    for raw in [
        json!({}),
        json!([]),
        json!({"data":null}),
        json!({"data":{}}),
        json!({"data":[{}]}),
        json!({"data":[{"id":42}]}),
        json!({"data":[{"id":""}]}),
        json!({"data":[{"id":"bad id"}]}),
        json!({"data":[{"id":"bad\u{1b}id"}]}),
        json!({"data":[{"id":"bad\u{a0}id"}]}),
        json!({"data":[{"id":"x".repeat(1025)}]}),
    ] {
        bodies.push(serde_json::to_vec(&raw).unwrap());
    }
    for body in bodies {
        let (client, _, server) = fixture(200, body).await;
        let error = client.models(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind, "protocol");
        assert!(!error.message.contains('\u{1b}'));
        assert_eq!(server.await.unwrap(), 1);
    }
}
#[tokio::test]
async fn catalog_accepts_explicit_empty_data_but_rejects_excessive_count_and_bytes() {
    let (client, _, server) = fixture(200, b"{\"data\":[]}".to_vec()).await;
    assert!(
        client
            .models(CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(server.await.unwrap(), 1);
    for body in [
        serde_json::to_vec(&json!({"data":vec![json!({"id":"m"});10001]})).unwrap(),
        vec![b' '; 2 * 1024 * 1024 + 1],
    ] {
        let (client, _, server) = fixture(200, body).await;
        assert_eq!(
            client
                .models(CancellationToken::new())
                .await
                .unwrap_err()
                .kind,
            "input_limit"
        );
        assert_eq!(server.await.unwrap(), 1);
    }
}
#[tokio::test]
async fn catalog_cancellation_interrupts_headers_and_incomplete_body() {
    for send_headers in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = HttpModelClient::new(connection(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();
        let (tx, rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request_head(&mut stream).await;
            if send_headers {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                    .await
                    .unwrap();
            }
            let _ = tx.send(());
            std::future::pending::<()>().await;
        });
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move { client.models(cancel).await }
        });
        rx.await.unwrap();
        cancel.cancel();
        let error = crate::test_watchdog::watchdog(
            "tests/model_catalog.rs: catalog_cancellation_interrupts_headers_and_incomplete_body",
            task,
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind, "cancelled");
        server.abort();
    }
}
#[tokio::test]
async fn catalog_precancelled_makes_no_http_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = HttpModelClient::new(connection(format!(
        "http://{}",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(client.models(cancel).await.unwrap_err().kind, "cancelled");
    assert!(futures_util::FutureExt::now_or_never(listener.accept()).is_none());
}

use crate::count_after;
