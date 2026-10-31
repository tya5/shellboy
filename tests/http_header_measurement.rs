//! Opt-in loopback measurements, never a provider-capacity benchmark.
use serde_json::json;
use shellboy::{config::Connection, model::http::HttpModelClient};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "synthetic header latency measurement; run serially with --nocapture"]
async fn successful_small_and_large_request_header_latency() {
    const SAMPLES: usize = 8;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = HttpModelClient::new(Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        response_header_timeout_ms: 5_000,
        stream_idle_timeout_ms: 5_000,
        ..Default::default()
    })
    .unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..SAMPLES * 2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.set_nodelay(true).unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 65_536];
            loop {
                let count = socket.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0, "incomplete measured request");
                bytes.extend_from_slice(&chunk[..count]);
                assert!(bytes.len() < 4_000_000);
                if let Some(split) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                    let length: usize = String::from_utf8_lossy(&bytes[..split])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            let body = r#"{"id":"latency-fixture","status":"completed","output":[]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    for characters in [1_024, 3_480_000] {
        let mut observations = Vec::new();
        for _ in 0..SAMPLES {
            let request = json!({"model":"fixture", "input":[{"role":"user","content":"x".repeat(characters)}], "tools":[]});
            let response = crate::test_watchdog::watchdog("tests/http_header_measurement.rs: successful_small_and_large_request_header_latency",
                client.response(request, CancellationToken::new(), None),
            )
            .await
            .expect("bounded loopback measurement")
            .expect("successful loopback response");
            let observation = response.http_observation.expect("header observation");
            assert!(observation.header_elapsed_ms.is_some());
            assert_eq!(
                observation.body_source_bytes_consumed,
                observation.request_body_bytes
            );
            observations.push(observation);
        }
        let mut milliseconds = observations
            .iter()
            .map(|o| o.header_elapsed_ms.unwrap())
            .collect::<Vec<_>>();
        milliseconds.sort_unstable();
        println!(
            "HEADER_MEASUREMENT {}",
            json!({"environment":"synthetic loopback HTTP/1.1", "input_characters":characters,
                "samples":SAMPLES, "p50_ms":milliseconds[3], "max_ms":milliseconds[7],
                "observations":observations, "provider_requests":0})
        );
    }
    crate::test_watchdog::watchdog(
        "tests/http_header_measurement.rs: successful_small_and_large_request_header_latency",
        server,
    )
    .await
    .unwrap()
    .unwrap();
}
