//! Transport deadlines exercised against sockets that remain open deliberately.
use futures_util::future::join_all;
use serde_json::{Value, json};
use shellboy::{
    config::Connection,
    model::{http::HttpModelClient, response_request},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Duration, Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

async fn read_request(stream: &mut TcpStream) {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "client disconnected before request headers");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 16 * 1024);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}

fn headers(status: u16, content_type: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

async fn stalled(
    mut connection: Connection,
    initial: Vec<u8>,
) -> (HttpModelClient, oneshot::Receiver<()>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    connection.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let client = HttpModelClient::for_connection("deadline_fixture", connection).unwrap();
    let (ready, received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        stream.write_all(&initial).await.unwrap();
        let _ = ready.send(());
        std::future::pending::<()>().await;
    });
    (client, received, server)
}

fn completed() -> Value {
    json!({"id":"response_deadline_fixture","status":"completed","output":[{
        "type":"message","id":"message_deadline_fixture","role":"assistant",
        "status":"completed","content":[{"type":"output_text","text":"complete"}]
    }]})
}

fn terminal_frame() -> Vec<u8> {
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":completed()})
    )
    .into_bytes()
}

#[tokio::test]
async fn embeddings_deadline_bounds_headers_empty_partial_and_error_bodies() {
    timeout(Duration::from_secs(10), async {
        let mut empty = headers(200, "application/json");
        let mut partial = empty.clone();
        partial.extend_from_slice(b"{\"data\":[");
        let mut error = headers(503, "application/json");
        error.extend_from_slice(b"{\"error\":");
        let unauthorized = headers(401, "application/json");
        let fixtures = [
            Vec::new(),
            std::mem::take(&mut empty),
            partial,
            error,
            unauthorized,
        ];
        join_all(
            fixtures
                .into_iter()
                .enumerate()
                .map(|(index, initial)| async move {
                    let (client, _, server) = stalled(
                        Connection {
                            request_timeout_ms: 1_000,
                            ..Default::default()
                        },
                        initial,
                    )
                    .await;
                    let error = client
                        .embeddings(
                            "fixture",
                            &["synthetic".into()],
                            None,
                            CancellationToken::new(),
                        )
                        .await
                        .unwrap_err();
                    assert_eq!(error.kind, "timeout", "variant {index}: {error}");
                    if index == 3 {
                        assert_eq!(error.status, Some(503));
                    }
                    if index == 4 {
                        assert_eq!(error.status, Some(401));
                    }
                    assert!(
                        error
                            .message
                            .contains("connections.deadline_fixture.request_timeout_ms")
                    );
                    assert!(
                        error
                            .message
                            .contains(if index == 0 { "headers" } else { "body" })
                    );
                    server.abort();
                    let _ = server.await;
                }),
        )
        .await;
    })
    .await
    .expect("embedding requests escaped their deadline");
}

#[tokio::test]
async fn stream_timeout_retains_partial_text_and_public_model_events() {
    timeout(Duration::from_secs(10), async {
        let event = json!({"type":"response.output_text.delta","delta":"already received"});
        let mut initial = headers(200, "text/event-stream");
        initial.extend_from_slice(format!("data: {event}\n\n").as_bytes());
        let (client, _, server) = stalled(
            Connection {
                stream_idle_timeout_ms: 500,
                ..Default::default()
            },
            initial,
        )
        .await;
        let (sender, mut events) = mpsc::channel(4);
        let failure = client
            .response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                Some(sender),
            )
            .await
            .unwrap_err();
        assert_eq!(failure.error.kind, "timeout");
        assert_eq!(failure.partial_text, "already received");
        assert!(
            failure
                .error
                .message
                .contains("connections.deadline_fixture.stream_idle_timeout_ms")
        );
        assert_eq!(events.recv().await.unwrap(), event);
        assert!(
            events.recv().await.is_none(),
            "transport observations leaked into public model events"
        );
        server.abort();
        let _ = server.await;
    })
    .await
    .expect("partial stream escaped inactivity timeout");
}

#[tokio::test]
async fn heartbeat_chunks_keep_a_stream_alive_past_its_idle_and_request_limits() {
    timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = HttpModelClient::new(Connection {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            stream_idle_timeout_ms: 1_000,
            request_timeout_ms: 1_000,
            ..Default::default()
        })
        .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            stream
                .write_all(&headers(200, "text/event-stream"))
                .await
                .unwrap();
            for _ in 0..6 {
                sleep(Duration::from_millis(250)).await;
                stream.write_all(b": heartbeat\n\n").await.unwrap();
            }
            stream.write_all(&terminal_frame()).await.unwrap();
        });
        let started = Instant::now();
        let result = client
            .response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.text, "complete");
        assert!(started.elapsed() > Duration::from_millis(1_000));
        server.await.unwrap();
    })
    .await
    .expect("healthy heartbeat stream failed to complete");
}

#[tokio::test]
async fn disabled_stream_deadline_still_allows_cancellation() {
    timeout(Duration::from_secs(10), async {
        let (client, ready, server) = stalled(
            Connection {
                stream_idle_timeout_ms: 0,
                request_timeout_ms: 1_000,
                ..Default::default()
            },
            headers(200, "text/event-stream"),
        )
        .await;
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                client
                    .response(
                        response_request("fixture", vec![], None, false),
                        cancel,
                        None,
                    )
                    .await
            }
        });
        ready.await.unwrap();
        sleep(Duration::from_millis(1_200)).await;
        assert!(
            !task.is_finished(),
            "request deadline was applied to an unlimited stream"
        );
        cancel.cancel();
        let failure = timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.error.kind, "cancelled");
        server.abort();
        let _ = server.await;
    })
    .await
    .expect("disabled deadline prevented cancellation");
}

#[tokio::test]
async fn configured_model_catalog_deadline_covers_an_incomplete_body() {
    timeout(Duration::from_secs(10), async {
        let mut initial = headers(200, "application/json");
        initial.extend_from_slice(b"{\"data\":[");
        let (client, _, server) = stalled(
            Connection {
                request_timeout_ms: 1_000,
                ..Default::default()
            },
            initial,
        )
        .await;
        let error = client.models(CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.kind, "timeout");
        assert!(error.message.contains("limit 1000 ms"));
        assert!(
            error
                .message
                .contains("connections.deadline_fixture.request_timeout_ms")
        );
        server.abort();
        let _ = server.await;
    })
    .await
    .expect("catalog ignored its configured deadline");
}

#[tokio::test]
async fn separate_connections_do_not_share_stream_deadlines() {
    timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection = Connection {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            stream_idle_timeout_ms: 500,
            ..Default::default()
        };
        let fast = HttpModelClient::for_connection("short", connection.clone()).unwrap();
        let slow = HttpModelClient::for_connection(
            "long",
            Connection {
                stream_idle_timeout_ms: 2_500,
                ..connection
            },
        )
        .unwrap();
        let server = tokio::spawn(async move {
            let mut tasks = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                tasks.push(tokio::spawn(async move {
                    read_request(&mut stream).await;
                    stream
                        .write_all(&headers(200, "text/event-stream"))
                        .await
                        .unwrap();
                    sleep(Duration::from_millis(1_200)).await;
                    // The short-deadline client may already have closed its socket.
                    let _ = stream.write_all(&terminal_frame()).await;
                }));
            }
            for task in tasks {
                task.await.unwrap();
            }
        });
        let (short_result, long_result) = tokio::join!(
            fast.response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                None
            ),
            slow.response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                None
            ),
        );
        let failure = short_result.unwrap_err();
        assert_eq!(failure.error.kind, "timeout");
        assert!(
            failure
                .error
                .message
                .contains("connections.short.stream_idle_timeout_ms")
        );
        assert_eq!(long_result.unwrap().text, "complete");
        server.await.unwrap();
    })
    .await
    .expect("per-connection deadline fixture failed to complete");
}
