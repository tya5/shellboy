//! Responses transport facts are bounded observations, never peer-delivery claims.
use serde_json::{Value, json};
use shellboy::{
    config::Connection,
    model::{
        http::{ConnectionReuse, HttpModelClient, HttpPhase},
        response_request,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Duration, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

async fn read_body(stream: &mut TcpStream) {
    let mut bytes = Vec::new();
    let mut chunk = [0; 65536];
    loop {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let len = String::from_utf8_lossy(&bytes[..at])
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= at + 4 + len {
                return;
            }
        }
    }
}
fn request() -> Value {
    response_request(
        "fixture",
        vec![json!({"role":"user","content":"synthetic"})],
        None,
        false,
    )
}
fn answer() -> String {
    json!({"id":"fixture","status":"completed","output":[]}).to_string()
}
fn head(status: u16, len: usize, keep: bool) -> String {
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: {}\r\n\r\n",
        if keep { "keep-alive" } else { "close" }
    )
}
fn client(listener: &TcpListener, header: u64, idle: u64) -> HttpModelClient {
    HttpModelClient::for_connection(
        "observed",
        Connection {
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            response_header_timeout_ms: header,
            stream_idle_timeout_ms: idle,
            ..Default::default()
        },
    )
    .unwrap()
}

#[tokio::test]
async fn header_deadline_records_source_consumption_without_claiming_peer_delivery() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 100, 1000);
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        read_body(&mut s).await;
        sleep(Duration::from_secs(2)).await;
    });
    let failure = timeout(
        Duration::from_secs(3),
        client.response(request(), CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(failure.error.kind, "timeout");
    assert!(
        failure
            .error
            .message
            .contains("connections.observed.response_header_timeout_ms")
    );
    let obs = failure.error.http_observation.as_deref().unwrap();
    assert_eq!(obs.phase, HttpPhase::AwaitingHeadersDeliveryUnknown);
    assert_eq!(obs.body_source_bytes_consumed, obs.request_body_bytes);
    assert!(obs.header_elapsed_ms.is_none());
    assert_eq!(obs.connection_reuse, ConnectionReuse::FreshPool);
    assert!(
        failure
            .error
            .message
            .contains("peer delivery and processing are unknown")
    );
    assert_eq!(failure.facts.http_observation, Some(*obs));
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn header_deadline_does_not_cut_post_header_silence_and_success_has_latency() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 200, 2000);
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        s.set_nodelay(true).unwrap();
        read_body(&mut s).await;
        let body = answer();
        s.write_all(head(200, body.len(), false).as_bytes())
            .await
            .unwrap();
        sleep(Duration::from_millis(350)).await;
        s.write_all(body.as_bytes()).await.unwrap();
    });
    let response = timeout(
        Duration::from_secs(3),
        client.response(request(), CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap();
    let obs = response.http_observation.unwrap();
    assert_eq!(obs.phase, HttpPhase::BodyReceive);
    assert!(obs.header_elapsed_ms.is_some());
    assert_eq!(obs.connection_reuse, ConnectionReuse::FreshPool);
    server.await.unwrap();
}
#[tokio::test]
async fn body_stall_records_received_headers_and_uses_body_idle_setting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 1000, 100);
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        s.set_nodelay(true).unwrap();
        read_body(&mut s).await;
        s.write_all(head(200, 100, false).as_bytes()).await.unwrap();
        sleep(Duration::from_secs(2)).await;
    });
    let failure = timeout(
        Duration::from_secs(3),
        client.response(request(), CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        failure
            .error
            .message
            .contains("connections.observed.stream_idle_timeout_ms")
    );
    let obs = failure.error.http_observation.unwrap();
    assert_eq!(obs.phase, HttpPhase::BodyReceive);
    assert!(obs.header_elapsed_ms.is_some());
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn stopped_request_reader_records_unconsumed_source_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 2000, 1000);
    let (read_prefix, prefix_received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut prefix = [0; 1024];
        let count = s.read(&mut prefix).await.unwrap();
        assert!(count > 0);
        read_prefix.send(count).unwrap();
        sleep(Duration::from_secs(5)).await;
    });
    let large = response_request(
        "fixture",
        vec![json!({"role":"user","content":"x".repeat(16*1024*1024)})],
        None,
        false,
    );
    let failure = timeout(
        Duration::from_secs(5),
        client.response(large, CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        prefix_received.await.unwrap() > 0,
        "server must actually read a request prefix before stopping"
    );
    let obs = failure.error.http_observation.as_deref().unwrap();
    assert_eq!(obs.phase, HttpPhase::UploadSourceIncomplete);
    assert!(obs.body_source_bytes_consumed < obs.request_body_bytes);
    assert!(
        failure
            .error
            .message
            .contains("provider processing is unknown")
    );
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn failed_attempt_retires_keepalive_pool_and_retry_uses_fresh_generation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 2000, 2000);
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        first.set_nodelay(true).unwrap();
        read_body(&mut first).await;
        let body = r#"{"error":{"code":"temporary","message":"fixture"}}"#;
        first
            .write_all(format!("{}{body}", head(500, body.len(), true)).as_bytes())
            .await
            .unwrap();
        let mut byte = [0; 1];
        assert_eq!(
            timeout(Duration::from_secs(2), first.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0,
            "retired idle connection remains open"
        );
        let (mut second, _) = listener.accept().await.unwrap();
        second.set_nodelay(true).unwrap();
        read_body(&mut second).await;
        let body = answer();
        second
            .write_all(format!("{}{body}", head(200, body.len(), false)).as_bytes())
            .await
            .unwrap();
    });
    let failure = client
        .response(request(), CancellationToken::new(), None)
        .await
        .unwrap_err();
    let old = failure.error.http_observation.as_deref().unwrap();
    assert_eq!(old.pool_generation, 0);
    let response = client
        .response(request(), CancellationToken::new(), None)
        .await
        .unwrap();
    let next = response.http_observation.unwrap();
    assert_eq!(next.pool_generation, 1);
    assert_eq!(next.connection_reuse, ConnectionReuse::FreshPool);
    server.await.unwrap();
}

#[tokio::test]
async fn connection_refused_is_before_body_source_and_not_a_partial_upload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 1000, 1000);
    drop(listener);
    for _ in 0..3 {
        let failure = timeout(
            Duration::from_secs(3),
            client.response(request(), CancellationToken::new(), None),
        )
        .await
        .unwrap()
        .unwrap_err();
        let observed = failure.error.http_observation.as_deref().unwrap();
        assert_eq!(observed.phase, HttpPhase::BeforeBodySource);
        assert_eq!(observed.body_source_bytes_consumed, 0);
        assert!(!observed.is_known_upload_incomplete());
        assert!(
            failure
                .error
                .message
                .contains("no partial upload was observed")
        );
    }
}
#[tokio::test]
async fn completed_unauthorized_response_does_not_retire_a_reusable_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 2000, 2000);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        read_body(&mut stream).await;
        let body = r#"{"error":{"code":"unauthorized","message":"fixture"}}"#;
        stream
            .write_all(format!("{}{body}", head(401, body.len(), true)).as_bytes())
            .await
            .unwrap();
        read_body(&mut stream).await;
        let body = answer();
        stream
            .write_all(format!("{}{body}", head(200, body.len(), false)).as_bytes())
            .await
            .unwrap();
    });
    let failure = client
        .response(request(), CancellationToken::new(), None)
        .await
        .unwrap_err();
    assert_eq!(failure.error.status, Some(401));
    let response = timeout(
        Duration::from_secs(3),
        client.response(request(), CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap();
    let obs = response.http_observation.unwrap();
    assert_eq!(obs.pool_generation, 0);
    assert_eq!(obs.connection_reuse, ConnectionReuse::Unknown);
    server.await.unwrap();
}
#[tokio::test]
async fn model_list_and_embedding_transport_errors_do_not_retire_responses_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 2000, 2000);
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_body(&mut stream).await;
            drop(stream);
        }
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.set_nodelay(true).unwrap();
        read_body(&mut stream).await;
        let body = answer();
        stream
            .write_all(format!("{}{body}", head(200, body.len(), false)).as_bytes())
            .await
            .unwrap();
    });
    assert!(client.models(CancellationToken::new()).await.is_err());
    assert!(
        client
            .embeddings(
                "fixture",
                &["synthetic".into()],
                None,
                None,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let response = timeout(
        Duration::from_secs(3),
        client.response(request(), CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap();
    let obs = response.http_observation.unwrap();
    assert_eq!(obs.pool_generation, 0);
    assert_eq!(obs.connection_reuse, ConnectionReuse::Unknown);
    server.await.unwrap();
}
#[tokio::test]
async fn cancelling_a_responses_request_does_not_retire_its_profile_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&listener, 2000, 2000);
    let (ready, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        read_body(&mut first).await;
        ready.send(()).unwrap();
        let (mut next, _) = listener.accept().await.unwrap();
        next.set_nodelay(true).unwrap();
        read_body(&mut next).await;
        let body = answer();
        next.write_all(format!("{}{body}", head(200, body.len(), false)).as_bytes())
            .await
            .unwrap();
    });
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let client = client.clone();
        let cancel = cancel.clone();
        async move { client.response(request(), cancel, None).await }
    });
    received.await.unwrap();
    cancel.cancel();
    assert_eq!(task.await.unwrap().unwrap_err().error.kind, "cancelled");
    let response = timeout(
        Duration::from_secs(3),
        client.response(request(), CancellationToken::new(), None),
    )
    .await
    .unwrap()
    .unwrap();
    let obs = response.http_observation.unwrap();
    assert_eq!(obs.pool_generation, 0);
    assert_eq!(obs.connection_reuse, ConnectionReuse::Unknown);
    server.await.unwrap();
}
