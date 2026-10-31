use serde_json::{Value, json};
use shellboy::{
    config::Connection,
    model::{http::HttpModelClient, response_request},
};
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Request {
    head: String,
    body: Value,
}
async fn read_request(stream: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let split = loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(split) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break split + 4;
        }
    };
    let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
        .unwrap();
    while bytes.len() < split + length {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    Request {
        head,
        body: serde_json::from_slice(&bytes[split..split + length]).unwrap(),
    }
}
async fn fake(
    status: u16,
    content_type: &str,
    body: Vec<u8>,
    fragment: usize,
) -> (
    HttpModelClient,
    oneshot::Receiver<Request>,
    count_after::CountAfter,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1/", listener.local_addr().unwrap()),
        ssl_verify: true,
        api_key_env: None,
        default_effort: None,
        headers: BTreeMap::from([("Authorization".into(), "Bearer fake-private-key".into())]),
        ..Default::default()
    };
    let (tx, rx) = oneshot::channel();
    let (finished, completion) = oneshot::channel();
    let content_type = content_type.to_owned();
    let task = tokio::spawn(async move {
        let Ok(Ok((mut stream, _))) =
            crate::test_watchdog::watchdog("tests/model_http.rs: fake", listener.accept()).await
        else {
            return 0;
        };
        let request = read_request(&mut stream).await;
        let _ = tx.send(request);
        let head = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if stream.write_all(head.as_bytes()).await.is_ok() {
            for chunk in body.chunks(fragment.max(1)) {
                if stream.write_all(chunk).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        drop(stream);
        let _ = completion.await;
        if futures_util::FutureExt::now_or_never(listener.accept()).is_some() {
            2
        } else {
            1
        }
    });
    (
        HttpModelClient::new(connection).unwrap(),
        rx,
        count_after::CountAfter::new(task, finished),
    )
}
fn final_response(text: &str) -> Value {
    json!({"id":"resp_fixture","status":"completed","output":[{"type":"message","id":"msg_fixture","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":text}]}]})
}
fn sse(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\r\ndata: {event}\r\n\r\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>()
        .into_bytes()
}
#[tokio::test]
async fn fragmented_sse_preserves_request_and_final_and_does_not_retry() {
    let raw = final_response("日本語の回答");
    let item = raw["output"][0].clone();
    let values = vec![
        json!({"type":"response.output_text.delta","delta":"日本語の回答"}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":raw}),
    ];
    let (client, request, server) =
        fake(200, "text/event-stream; charset=utf-8", sse(&values), 1).await;
    let (tx, mut rx) = mpsc::channel(1);
    let consumer = tokio::spawn(async move {
        let mut result = Vec::new();
        while let Some(value) = rx.recv().await {
            result.push(value);
        }
        result
    });
    let result = client
        .response(
            response_request("model-fixture", vec![], Some("future-effort"), true),
            CancellationToken::new(),
            Some(tx),
        )
        .await
        .unwrap();
    assert_eq!(result.text, "日本語の回答");
    assert_eq!(result.raw["output"][0]["phase"], "commentary");
    assert_eq!(consumer.await.unwrap(), values);
    let request = request.await.unwrap();
    assert!(request.head.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(
        request
            .head
            .to_ascii_lowercase()
            .contains("authorization: bearer fake-private-key")
    );
    assert_eq!(request.body["store"], false);
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["reasoning"]["effort"], "future-effort");
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn eof_retains_partial_text_and_raw_done_items() {
    let item = final_response("partial")["output"][0].clone();
    let body = sse(&[
        json!({"type":"response.output_text.delta","delta":"partial"}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
    ]);
    let (client, _, server) = fake(200, "text/event-stream", body, 3).await;
    let failure = client
        .response(
            response_request("m", vec![], None, false),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error.kind, "incomplete");
    assert_eq!(failure.partial_text, "partial");
    assert_eq!(failure.completed_items[&0], item);
    assert!(failure.facts.received == shellboy::model::http::ResponseReceived::Output);
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn provider_errors_keep_explicit_code_and_never_infer_context_or_retry() {
    for (code, message, context) in [
        ("context_length_exceeded", "too many tokens", true),
        ("proxy_rejected", "context too large maybe", false),
    ] {
        let body = serde_json::to_vec(&json!({"error":{"code":code,"message":message}})).unwrap();
        let (client, _, server) = fake(400, "application/json", body, 7).await;
        let failure = client
            .response(
                response_request("m", vec![], None, false),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(failure.error.provider_code.as_deref(), Some(code));
        assert_eq!(failure.error.message, message);
        assert_eq!(failure.error.status, Some(400));
        assert_eq!(failure.error.is_context_limit(), context);
        assert_eq!(failure.facts.status, Some(400));
        assert_eq!(server.await.unwrap(), 1);
    }
}
#[tokio::test]
async fn error_body_is_bounded_and_secret_header_values_are_redacted() {
    let (client, _, server) = fake(
        502,
        "text/html",
        format!("upstream fake-private-key {}", "z".repeat(100_000)).into_bytes(),
        4096,
    )
    .await;
    let failure = client
        .response(
            response_request("m", vec![], None, false),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error.kind, "http");
    assert!(failure.error.message.contains("truncated"));
    assert!(!failure.error.message.contains("fake-private-key"));
    assert!(failure.error.message.len() < 17_000);
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn json_transport_normalization_and_unsupported_output_preservation() {
    let raw = final_response("JSON final");
    let (client, _, server) = fake(
        200,
        "application/json",
        serde_json::to_vec(&raw).unwrap(),
        4,
    )
    .await;
    let result = client
        .response(
            json!({"model":"m","stream":false,"store":true}),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.text, "JSON final");
    assert_eq!(server.await.unwrap(), 1);
    let raw =
        json!({"status":"completed","output":[{"type":"future_new_item","opaque":"retain me"}]});
    let (client, _, server) = fake(
        200,
        "text/event-stream",
        sse(&[json!({"type":"response.completed","response":raw})]),
        100,
    )
    .await;
    let failure = client
        .response(
            response_request("m", vec![], None, false),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error.kind, "unsupported_item");
    assert_eq!(failure.raw_response.unwrap(), raw);
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn cancelled_before_start_and_missing_credentials_make_no_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Connection::default()
    };
    let client = HttpModelClient::new(connection.clone()).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let failure = client
        .response(response_request("m", vec![], None, false), cancel, None)
        .await
        .unwrap_err();
    assert_eq!(failure.error.kind, "cancelled");
    let missing = format!("SHELLBOY_TEST_MISSING_{}", uuid::Uuid::new_v4().simple());
    let client = HttpModelClient::new(Connection {
        api_key_env: Some(missing),
        ..connection
    })
    .unwrap();
    let failure = client
        .response(
            response_request("m", vec![], None, false),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error.kind, "config_missing");
    assert!(futures_util::FutureExt::now_or_never(listener.accept()).is_none());
}
#[tokio::test]
async fn cancellation_interrupts_bounded_event_backpressure_and_preserves_partial() {
    let body = sse(&[
        json!({"type":"response.output_text.delta","delta":"one"}),
        json!({"type":"response.output_text.delta","delta":"two"}),
        json!({"type":"response.completed","response":final_response("onetwo")}),
    ]);
    let (client, request, server) = fake(200, "text/event-stream", body, 1024).await;
    let (tx, mut rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let attempt_cancel = cancel.clone();
    let attempt = tokio::spawn(async move {
        client
            .response(
                response_request("m", vec![], None, false),
                attempt_cancel,
                Some(tx),
            )
            .await
    });
    request.await.unwrap();
    let first = crate::test_watchdog::watchdog("tests/model_http.rs: cancellation_interrupts_bounded_event_backpressure_and_preserves_partial", rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first["delta"], "one");
    cancel.cancel();
    let failure = crate::test_watchdog::watchdog("tests/model_http.rs: cancellation_interrupts_bounded_event_backpressure_and_preserves_partial", attempt)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.error.kind, "cancelled");
    assert!(failure.partial_text.starts_with("one"));
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn closed_consumer_is_an_explicit_failure() {
    let (client, _, server) = fake(
        200,
        "text/event-stream",
        sse(&[json!({"type":"response.completed","response":final_response("done")})]),
        100,
    )
    .await;
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    let failure = client
        .response(
            response_request("m", vec![], None, false),
            CancellationToken::new(),
            Some(tx),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error.kind, "consumer_closed");
    assert!(failure.raw_response.is_some());
    assert_eq!(server.await.unwrap(), 1);
}
#[tokio::test]
async fn embeddings_are_sorted_validated_and_dimension_is_finite() {
    let raw = json!({"data":[{"index":1,"embedding":[3.0,4.0]},{"index":0,"embedding":[1.0,2.0]}],"usage":{"total_tokens":8}});
    let (client, request, server) = fake(
        200,
        "application/json",
        serde_json::to_vec(&raw).unwrap(),
        5,
    )
    .await;
    let result = client
        .embeddings(
            "embed",
            &["first".into(), "second".into()],
            None,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.vectors, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    assert_eq!(result.dimension, 2);
    let request = request.await.unwrap();
    assert!(request.head.starts_with("POST /v1/embeddings HTTP/1.1"));
    assert_eq!(request.body["encoding_format"], "float");
    assert_eq!(server.await.unwrap(), 1);
    for (raw, expected, kind) in [
        (
            json!({"data":[{"index":0,"embedding":[1.0,2.0]}]}),
            Some(3),
            "dimension_mismatch",
        ),
        (
            json!({"data":[{"index":0,"embedding":[1e100]}]}),
            None,
            "protocol",
        ),
        (
            json!({"data":[{"index":1,"embedding":[1.0]}]}),
            None,
            "protocol",
        ),
    ] {
        let (client, _, server) = fake(
            200,
            "application/json",
            serde_json::to_vec(&raw).unwrap(),
            100,
        )
        .await;
        let error = client
            .embeddings(
                "embed",
                &["one".into()],
                expected,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(server.await.unwrap(), 1);
    }
}

#[tokio::test]
async fn native_embedding_request_omits_dimensions_and_preserves_exact_fields() {
    let (client, request, server) = fake(
        200,
        "application/json",
        serde_json::to_vec(&json!({"data":[{"index":0,"embedding":[1.0,2.0]}]})).unwrap(),
        128,
    )
    .await;
    let result = client
        .embeddings(
            "embed",
            &["one".into()],
            Some(2),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.dimension, 2);
    assert_eq!(
        request.await.unwrap().body,
        json!({"model":"embed","input":["one"],"encoding_format":"float"})
    );
    assert_eq!(server.await.unwrap(), 1);
}

#[tokio::test]
async fn requested_embedding_dimension_is_sent_and_validated_without_fallback() {
    for (returned, succeeds) in [(2, true), (3, false)] {
        let raw = json!({"data":[{"index":0,"embedding":vec![1.0;returned]}]});
        let (client, request, server) = fake(
            200,
            "application/json",
            serde_json::to_vec(&raw).unwrap(),
            128,
        )
        .await;
        // A probe has no previously observed dimension, but still requires N.
        let result = client
            .embeddings(
                "embed",
                &["one".into()],
                None,
                Some(2),
                CancellationToken::new(),
            )
            .await;
        assert_eq!(
            request.await.unwrap().body,
            json!({"model":"embed","input":["one"],"encoding_format":"float","dimensions":2})
        );
        if succeeds {
            assert_eq!(result.unwrap().vectors, vec![vec![1.0, 1.0]]);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, "dimension_mismatch");
            assert!(error.message.contains("length 3"));
            assert!(error.message.contains("dimension 2"));
        }
        assert_eq!(server.await.unwrap(), 1, "no native-dimension fallback");
    }
    let (client, request, server) = fake(
        200,
        "application/json",
        serde_json::to_vec(
            &json!({"data":[{"index":1,"embedding":[3.0,4.0]},{"index":0,"embedding":[1.0,2.0]}]}),
        )
        .unwrap(),
        128,
    )
    .await;
    let result = client
        .embeddings(
            "embed",
            &["one".into(), "two".into()],
            Some(2),
            Some(2),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.vectors, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    let request = request.await.unwrap();
    assert_eq!(request.body["dimensions"], 2);
    assert_eq!(request.body["input"], json!(["one", "two"]));
    assert_eq!(server.await.unwrap(), 1);

    let (client, request, server) = fake(
        400,
        "application/json",
        serde_json::to_vec(&json!({"error":{"code":"unsupported_dimensions","message":"fixture refuses dimensions"}})).unwrap(),
        128,
    )
    .await;
    let error = client
        .embeddings(
            "embed",
            &["one".into()],
            None,
            Some(2),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, Some(400));
    assert_eq!(
        error.provider_code.as_deref(),
        Some("unsupported_dimensions")
    );
    assert_eq!(request.await.unwrap().body["dimensions"], 2);
    assert_eq!(
        server.await.unwrap(),
        1,
        "refusal never retries without dimensions"
    );
}

#[tokio::test]
async fn zero_or_conflicting_embedding_dimensions_fail_before_http() {
    // A synchronous nonblocking accept proves absence without waiting on time.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let client = HttpModelClient::new(Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Default::default()
    })
    .unwrap();
    for (expected, requested) in [(None, Some(0)), (Some(0), None), (Some(2), Some(3))] {
        let error = client
            .embeddings(
                "embed",
                &["one".into()],
                expected,
                requested,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, "input_invalid");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[tokio::test]
async fn embedding_batches_reject_misaligned_or_partially_invalid_vectors_atomically() {
    let cases = [
        (json!({"data":[{"index":0,"embedding":[1.0]}]}), "protocol"),
        (
            json!({"data":[{"index":0,"embedding":[1.0]},{"index":0,"embedding":[2.0]}]}),
            "protocol",
        ),
        (
            json!({"data":[{"index":0,"embedding":[1.0]},{"index":2,"embedding":[2.0]}]}),
            "protocol",
        ),
        (
            json!({"data":[{"index":0,"embedding":[1.0]},{"index":1.5,"embedding":[2.0]}]}),
            "protocol",
        ),
        (
            json!({"data":[{"index":0,"embedding":[1.0]},{"index":-1,"embedding":[2.0]}]}),
            "protocol",
        ),
        (
            json!({"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[]}]}),
            "protocol",
        ),
        (
            json!({"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[2.0,3.0]}]}),
            "dimension_mismatch",
        ),
        (
            json!({"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[1e100]}]}),
            "protocol",
        ),
    ];
    for (raw, kind) in cases {
        let (client, _, server) = fake(
            200,
            "application/json",
            serde_json::to_vec(&raw).unwrap(),
            128,
        )
        .await;
        let error = client
            .embeddings(
                "embed",
                &["one".into(), "two".into()],
                None,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind, "{raw}");
        assert_eq!(server.await.unwrap(), 1);
    }
}

#[tokio::test]
async fn cancellation_interrupts_a_stalled_http_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..Connection::default()
    };
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000000\r\n\r\n").await.unwrap();
        stream
            .write_all(&sse(&[
                json!({"type":"response.output_text.delta","delta":"before cancellation"}),
            ]))
            .await
            .unwrap();
        let _ = released.await;
    });
    let client = HttpModelClient::new(connection).unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let attempt_cancel = cancel.clone();
    let attempt = tokio::spawn(async move {
        client
            .response(
                response_request("m", vec![], None, false),
                attempt_cancel,
                Some(tx),
            )
            .await
    });
    assert_eq!(
        crate::test_watchdog::watchdog(
            "tests/model_http.rs: cancellation_interrupts_a_stalled_http_stream",
            rx.recv()
        )
        .await
        .unwrap()
        .unwrap()["delta"],
        "before cancellation"
    );
    cancel.cancel();
    let failure = crate::test_watchdog::watchdog(
        "tests/model_http.rs: cancellation_interrupts_a_stalled_http_stream",
        attempt,
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap_err();
    assert_eq!(failure.error.kind, "cancelled");
    assert_eq!(failure.partial_text, "before cancellation");
    release.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn original_sse_provider_error_survives_coalesced_trailing_partial_frame() {
    let body = b"data: {\"type\":\"error\",\"code\":\"credit_balance_exhausted\",\"message\":\"Synthetic no credits\"}\n\ndata: {partial".to_vec();
    let (client, request, server) = fake(200, "text/event-stream", body, usize::MAX).await;
    let failure = client
        .response(
            json!({"model":"fixture","input":"synthetic"}),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        failure.error.provider_code.as_deref(),
        Some("credit_balance_exhausted")
    );
    assert_eq!(failure.error.message, "Synthetic no credits");
    request.await.unwrap();
    assert_eq!(server.await.unwrap(), 1);
}

#[tokio::test]
async fn http_error_body_marks_truncation_only_after_an_observed_excess_byte() {
    for (size, truncated) in [(65_535, false), (65_536, false), (65_537, true)] {
        let mut body =
            serde_json::to_vec(&json!({"error":{"code":"fixture","message":"bounded failure"}}))
                .unwrap();
        body.resize(size, b' ');
        let (client, _, server) = fake(502, "application/json", body, 4096).await;
        let failure = client
            .response(
                response_request("m", vec![], None, false),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(failure.error.status, Some(502));
        assert_eq!(failure.error.provider_code.as_deref(), Some("fixture"));
        assert_eq!(
            failure.error.message.contains("HTTP error body truncated"),
            truncated,
            "body size={size}: {}",
            failure.error.message
        );
        assert_eq!(server.await.unwrap(), 1);
    }
}

#[tokio::test]
async fn malformed_terminal_keeps_latest_raw_answer_reasoning_and_completed_items() {
    let item = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"paid received text"}]});
    let raw = json!({"id":"bad-final","status":"completed","output":null});
    let body = sse(&[
        json!({"type":"response.created","response":{"id":"old-created","status":"in_progress"}}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"paid reasoning"}),
        json!({"type":"response.output_text.delta","delta":"paid received text"}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":raw}),
    ]);
    let (client, _, task) = fake(200, "text/event-stream", body, 7).await;
    let failure = client
        .response(
            response_request("fixture", vec![], None, true),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error.kind, "protocol");
    assert_eq!(failure.raw_response, Some(raw));
    assert_eq!(failure.partial_text, "paid received text");
    assert_eq!(failure.partial_reasoning, "paid reasoning");
    assert_eq!(failure.completed_items[&0], item);
    assert_eq!(task.await.unwrap(), 1, "no automatic retry");
}

#[tokio::test]
async fn differing_stream_tool_payload_uses_only_valid_final_arguments_without_retry() {
    let old = json!({"type":"function_call","id":"old-id","call_id":"call","name":"shell","arguments":"{\"command\":\"echo received\"}","status":"completed"});
    let final_item = json!({"type":"function_call","id":"final-id","call_id":"call","name":"shell","arguments":"{\"command\":\"echo final\"}","status":"completed"});
    let raw = json!({"id":"final-response","status":"completed","output":[final_item]});
    let body = sse(&[
        json!({"type":"response.output_item.done","output_index":0,"item":old}),
        json!({"type":"response.completed","response":raw}),
    ]);
    let (client, _, task) = fake(200, "text/event-stream", body, 3).await;
    let response = client
        .response(
            response_request("fixture", vec![], None, true),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(response.raw, raw);
    assert_eq!(response.calls.len(), 1);
    assert_eq!(
        response.calls[0].args.as_ref().unwrap().command,
        "echo final"
    );
    let observation = response.stream_observation.unwrap();
    assert_eq!(observation.completed_items[&0], old);
    assert_eq!(observation.total_differences, 1);
    assert_eq!(
        observation.differences[0].field,
        shellboy::model::OutputMismatchField::Arguments
    );
    assert_eq!(task.await.unwrap(), 1);
}

#[tokio::test]
async fn malformed_sse_preserves_valid_received_prefix_for_whole_and_fragmented_bodies() {
    let created = json!({"id":"created-before-error","status":"in_progress"});
    let item = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"paid done"}]} );
    let mut body = sse(&[
        json!({"type":"response.created","response":created}),
        json!({"type":"response.output_text.delta","delta":"paid answer"}),
        json!({"type":"response.reasoning_text.delta","delta":"paid reasoning"}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
    ]);
    body.extend_from_slice(b"data: {broken\n\n");
    for fragment in [1, body.len()] {
        let (client, _, task) = fake(200, "text/event-stream", body.clone(), fragment).await;
        let failure = client
            .response(
                response_request("fixture", vec![], None, true),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(failure.error.kind, "protocol");
        assert_eq!(failure.partial_text, "paid answer");
        assert_eq!(failure.partial_reasoning, "paid reasoning");
        assert_eq!(failure.completed_items[&0], item);
        assert_eq!(failure.raw_response, Some(created.clone()));
        assert_eq!(task.await.unwrap(), 1, "no automatic retry");
    }
}

#[tokio::test]
async fn delta_difference_with_equal_item_done_is_preserved_without_rejecting_final_response() {
    use shellboy::model::OutputMismatchField as Field;
    let item = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"final answer"}]} );
    let raw = json!({"id":"final-response","status":"completed","output":[item]});
    let body = sse(&[
        json!({"type":"response.output_text.delta","delta":"paid old answer"}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":raw}),
    ]);
    let (client, _, task) = fake(200, "text/event-stream", body, 3).await;
    let response = client
        .response(
            response_request("fixture", vec![], None, true),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(response.raw, raw);
    assert_eq!(response.text, "final answer");
    assert!(response.calls.is_empty());
    let observation = response.stream_observation.unwrap();
    assert!(observation.differences.is_empty());
    assert_eq!(observation.delta_differences, vec![Field::Text]);
    assert_eq!(observation.total_differences, 1);
    assert_eq!(observation.partial_text, "paid old answer");
    assert_eq!(task.await.unwrap(), 1);
}

#[tokio::test]
async fn valid_sse_framing_survives_immediate_http_eof_without_losing_completed() {
    let raw = final_response("frame completed before connection close");
    let completed = json!({"type":"response.completed","response":raw});
    for body in [
        format!("\u{feff}data: {completed}\n\n"),
        format!("data: {completed}\r\r"),
        format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"frame completed before connection close\"}}\n\ndata: {completed}\r\r"
        ),
    ] {
        let (client, _, server) = fake(200, "text/event-stream", body.into_bytes(), 1).await;
        let (tx, mut rx) = mpsc::channel(8);
        let result = client
            .response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                Some(tx),
            )
            .await
            .unwrap();
        assert_eq!(result.raw, raw);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        assert_eq!(events.last().unwrap(), &completed);
        assert_eq!(server.await.unwrap(), 1);
    }
}

#[tokio::test]
async fn done_marker_without_typed_terminal_stays_incomplete_and_has_safe_diagnostics() {
    let body = b"event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"output\":[]}}\n\ndata: [DONE]\n\n".to_vec();
    let (client, _, server) = fake(200, "text/event-stream", body, 1).await;
    let failure = client
        .response(
            response_request("fixture", vec![], None, false),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        failure
            .error
            .message
            .starts_with("EOF before response.completed")
    );
    assert!(failure.error.message.contains("parsed_events=1"));
    assert!(failure.error.message.contains("done_markers=1"));
    assert!(failure.error.message.contains("last_event=untyped"));
    assert_ne!(
        failure.facts.received,
        shellboy::model::http::ResponseReceived::Output
    );
    assert_eq!(server.await.unwrap(), 1);
}

#[tokio::test]
async fn eof_retry_facts_use_semantic_output_rather_than_received_bytes() {
    for (body, partial) in [
        (Vec::new(), false),
        (b": heartbeat\n\n".to_vec(), false),
        (b"data: [DONE]\n\n".to_vec(), false),
        (b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"fixture\",\"output\":[]}}\n\n".to_vec(), false),
        (sse(&[json!({"type":"response.in_progress","response":{"output":[]}})]), false),
        (sse(&[json!({"type":"response.reasoning_text.delta","delta":"thought"})]), true),
        (sse(&[json!({"type":"response.function_call_arguments.delta","delta":"{"})]), true),
        (sse(&[json!({"type":"response.output_item.added","item":{"type":"function_call"}})]), true),
    ] {
        let (client, _, server) = fake(200, "text/event-stream", body, 1).await;
        let failure = client.response(response_request("fixture", vec![], None, true), CancellationToken::new(), None).await.unwrap_err();
        assert_eq!(failure.error.kind, "incomplete");
        assert_eq!(failure.facts.received == shellboy::model::http::ResponseReceived::Output, partial);
        assert_eq!(server.await.unwrap(), 1, "HTTP client never owns retries");
    }
}

#[tokio::test]
async fn malformed_or_terminal_invalid_streams_never_become_interrupt_retries() {
    for body in [
        b"data: {invalid}\n\n".to_vec(),
        sse(&[
            json!({"type":"response.failed","response":{"error":{"code":"unavailable","message":"failed"}}}),
        ]),
        sse(&[
            json!({"type":"response.incomplete","response":{"status":"incomplete","output":[]}}),
        ]),
        sse(&[json!({"type":"response.completed","response":{"status":"completed"}})]),
        sse(&[
            json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"unknown"}]}}),
        ]),
        sse(&[
            json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","status":"in_progress","content":[]}]}}),
        ]),
    ] {
        let (client, _, server) = fake(200, "text/event-stream", body, 2).await;
        let failure = client
            .response(
                response_request("fixture", vec![], None, true),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            failure.facts.terminal_received || failure.error.kind == "protocol",
            "{}",
            failure.error
        );
        assert_eq!(server.await.unwrap(), 1);
    }
}

#[tokio::test]
async fn broken_http_body_preserves_stream_observations_and_retry_facts() {
    for (event, partial) in [
        (
            json!({"type":"response.created","response":{"id":"metadata","output":[]}}),
            false,
        ),
        (
            json!({"type":"response.output_text.delta","delta":"paid partial"}),
            true,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = HttpModelClient::new(Connection {
            base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
            ..Default::default()
        })
        .unwrap();
        let body = sse(&[event]);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            // Promise more HTTP bytes than are sent to force a transport-body
            // interruption independently of the valid SSE frame boundary.
            let head = format!(
                "HTTP/1.1 200 Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len() + 100
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
            stream.shutdown().await.unwrap();
        });
        let failure = client
            .response(
                response_request("fixture", vec![], None, true),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(failure.error.kind, "transport");
        assert_eq!(
            failure.facts.received == shellboy::model::http::ResponseReceived::Output,
            partial
        );
        assert_eq!(
            failure.partial_text,
            if partial { "paid partial" } else { "" }
        );
        if !partial {
            assert_eq!(failure.raw_response.unwrap()["id"], "metadata");
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn complete_json_validation_failure_is_a_response_boundary_not_interrupted_transport() {
    for raw in [
        json!({"status":"in_progress","output":[]}),
        json!({"status":"incomplete","output":[]}),
        json!({"output":[]}),
    ] {
        let (client, _, server) = fake(
            200,
            "application/json",
            serde_json::to_vec(&raw).unwrap(),
            1,
        )
        .await;
        let failure = client
            .response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(failure.error.kind, "incomplete");
        assert!(failure.facts.terminal_received);
        assert_eq!(failure.facts.status, Some(200));
        assert_eq!(failure.raw_response, Some(raw));
        assert_eq!(server.await.unwrap(), 1);
    }
}

struct SessionFixture {
    client: HttpModelClient,
    requests: mpsc::Receiver<Request>,
    accepts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    server: JoinHandle<()>,
}
impl Drop for SessionFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn session_fixture(mut connection: Connection) -> SessionFixture {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    connection.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let client = HttpModelClient::new(connection).unwrap();
    let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = accepts.clone();
    let (send, requests) = mpsc::channel(16);
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut reader = BufReader::new(stream);
            loop {
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    head.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                if head.is_empty() {
                    break;
                }
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut bytes = vec![0; length];
                if reader.read_exact(&mut bytes).await.is_err() {
                    break;
                }
                let body = if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                };
                let response = if head.starts_with("GET /v1/models ") {
                    json!({"data":[{"id":"fixture"}]})
                } else if head.starts_with("POST /v1/embeddings ") {
                    json!({"data":[{"index":0,"embedding":[1.0]}]})
                } else {
                    final_response("ok")
                };
                send.send(Request { head, body }).await.unwrap();
                let response = response.to_string();
                let wire = format!(
                    "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{response}",
                    response.len()
                );
                if reader.get_mut().write_all(wire.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });
    SessionFixture {
        client,
        requests,
        accepts,
        server,
    }
}
fn session_values(request: &Request) -> Vec<&str> {
    request
        .head
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("x-shellboy-session-id")
                .then_some(value.trim())
        })
        .collect()
}
fn session_request() -> Value {
    response_request("fixture", vec![], None, false)
}

#[tokio::test]
async fn automatic_session_is_per_request_stable_and_does_not_leak_through_a_pooled_client() {
    let mut fixture = session_fixture(Connection::default()).await;
    for thread in ["thread-a", "thread-a", "thread-b"] {
        fixture
            .client
            .clone()
            .response_for_thread(thread, session_request(), CancellationToken::new(), None)
            .await
            .unwrap();
        let request = fixture.requests.recv().await.unwrap();
        assert_eq!(session_values(&request), [thread]);
        assert!(request.body.get("prompt_cache_key").is_none());
    }
    fixture
        .client
        .response(session_request(), CancellationToken::new(), None)
        .await
        .unwrap();
    assert!(session_values(&fixture.requests.recv().await.unwrap()).is_empty());
    fixture
        .client
        .embeddings(
            "fixture",
            &["synthetic".into()],
            Some(1),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let embedding = fixture.requests.recv().await.unwrap();
    assert!(session_values(&embedding).is_empty());
    assert!(embedding.body.get("prompt_cache_key").is_none());
    fixture
        .client
        .models(CancellationToken::new())
        .await
        .unwrap();
    assert!(session_values(&fixture.requests.recv().await.unwrap()).is_empty());
    fixture
        .client
        .response_for_thread(
            "thread-a",
            session_request(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        session_values(&fixture.requests.recv().await.unwrap()),
        ["thread-a"]
    );
    assert_eq!(fixture.accepts.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn session_header_and_prompt_cache_key_can_be_selected_independently() {
    for (header, cache) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut fixture = session_fixture(Connection {
            session_header: header,
            prompt_cache_key: cache,
            ..Default::default()
        })
        .await;
        fixture
            .client
            .response_for_thread(
                "thread-switches",
                session_request(),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let request = fixture.requests.recv().await.unwrap();
        assert_eq!(
            session_values(&request),
            if header {
                vec!["thread-switches"]
            } else {
                vec![]
            }
        );
        assert_eq!(
            request.body.get("prompt_cache_key"),
            cache
                .then(|| Value::String("thread-switches".into()))
                .as_ref()
        );
        fixture
            .client
            .response_for_thread(
                "thread-other",
                session_request(),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let other = fixture.requests.recv().await.unwrap();
        assert_eq!(
            session_values(&other),
            if header { vec!["thread-other"] } else { vec![] }
        );
        assert_eq!(
            other.body.get("prompt_cache_key"),
            cache.then(|| Value::String("thread-other".into())).as_ref()
        );
        // A no-context call cannot inherit either automatic field.
        fixture
            .client
            .response(session_request(), CancellationToken::new(), None)
            .await
            .unwrap();
        let plain = fixture.requests.recv().await.unwrap();
        assert!(session_values(&plain).is_empty());
        assert!(plain.body.get("prompt_cache_key").is_none());
    }
}

#[tokio::test]
async fn automatic_session_overrides_case_insensitive_static_header_and_opt_out_preserves_it() {
    for auto in [true, false] {
        let mut fixture = session_fixture(Connection {
            session_header: auto,
            headers: BTreeMap::from([("X-Shellboy-Session-ID".into(), "static-session".into())]),
            ..Default::default()
        })
        .await;
        fixture
            .client
            .response_for_thread(
                "thread-dynamic",
                session_request(),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            session_values(&fixture.requests.recv().await.unwrap()),
            [if auto {
                "thread-dynamic"
            } else {
                "static-session"
            }]
        );
        // The static header is user configuration, so unrelated endpoints keep it.
        fixture
            .client
            .models(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            session_values(&fixture.requests.recv().await.unwrap()),
            ["static-session"]
        );
    }
}

#[tokio::test]
async fn automatic_session_rejects_invalid_context_before_io_without_echoing_it() {
    for (header, cache) in [(true, false), (false, true)] {
        let fixture = session_fixture(Connection {
            session_header: header,
            prompt_cache_key: cache,
            ..Default::default()
        })
        .await;
        for thread in [
            "private-session\r\nX-Injected: bad".to_owned(),
            "a".repeat(65),
            String::new(),
        ] {
            let failure = fixture
                .client
                .response_for_thread(&thread, session_request(), CancellationToken::new(), None)
                .await
                .unwrap_err();
            assert_eq!(failure.error.kind, "input_invalid");
            if !thread.is_empty() {
                assert!(!failure.error.message.contains(&thread));
            }
            assert_eq!(fixture.accepts.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }
    let mut disabled = session_fixture(Connection {
        session_header: false,
        prompt_cache_key: false,
        ..Default::default()
    })
    .await;
    disabled
        .client
        .response_for_thread(
            "unused\ncontext",
            session_request(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert!(session_values(&disabled.requests.recv().await.unwrap()).is_empty());
}

#[tokio::test]
async fn sse_terminal_usage_is_normalized_for_success_and_failed_envelopes_only() {
    for state in ["completed", "failed", "incomplete"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = HttpModelClient::new(Connection {
            base_url: format!("http://{}/v1/", listener.local_addr().unwrap()),
            ..Default::default()
        })
        .unwrap();
        let mut raw = final_response("unchanged body containing usage/attribution");
        raw["status"] = state.into();
        raw["usage"] = json!({"input_tokens":23,"input_tokens_details":{"cached_tokens":0,"extra":"x".repeat(128*1024)},"output_tokens":3,"attribution":"x".repeat(128*1024)});
        if state != "completed" {
            raw["error"] = json!({"code":"invalid_request_error","message":"unchanged diagnosis"});
        }
        let original = raw.clone();
        let body = sse(&[json!({"type":format!("response.{state}"),"response":raw})]);
        let server = tokio::spawn(async move {
            let (mut stream, _) = crate::test_watchdog::watchdog("tests/model_http.rs: sse_terminal_usage_is_normalized_for_success_and_failed_envelopes_only", listener.accept())
                .await
                .unwrap()
                .unwrap();
            let _ = read_request(&mut stream).await;
            stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let result = crate::test_watchdog::watchdog("tests/model_http.rs: sse_terminal_usage_is_normalized_for_success_and_failed_envelopes_only",
            client.response(
                response_request("fixture", vec![], None, false),
                CancellationToken::new(),
                None,
            ),
        )
        .await
        .unwrap();
        let raw = match result {
            Ok(response) => {
                assert_eq!(state, "completed");
                response.raw
            }
            Err(failure) => {
                assert_ne!(state, "completed");
                failure.raw_response.unwrap()
            }
        };
        assert_eq!(
            raw["usage"],
            json!({"input_tokens":23,"input_tokens_details":{"cached_tokens":0},"output_tokens":3})
        );
        assert_eq!(raw["output"], original["output"]);
        assert_eq!(raw["error"], original["error"]);
        assert_eq!(raw["id"], original["id"]);
        server.await.unwrap();
    }
}

use crate::count_after;
