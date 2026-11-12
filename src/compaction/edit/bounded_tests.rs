//! Loopback-only summary fixtures. Timeouts are watchdogs, never timing assertions.
use super::tests::Fixture;
use super::*;
use std::{fs::OpenOptions, io::Write, path::Path};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn args(to: &str) -> ContextEditArgs {
    ContextEditArgs {
        thread_id: Some("t".into()),
        action: ContextEditAction::Summary,
        from_node_id: Some("n0".into()),
        to_node_id: Some(to.into()),
        max_chars: None,
    }
}
fn lines(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap().lines().count()
}
async fn setup(responses: Vec<String>, log: &Path) -> (Fixture, tokio::task::JoinHandle<()>) {
    std::fs::write(log, "").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = crate::config::Connection {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        stream_idle_timeout_ms: 0,
        ..Default::default()
    };
    let log = log.to_owned();
    let server = tokio::spawn(async move {
        for text in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let request = loop {
                let mut buf = [0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..at]);
                    let size: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= at + 4 + size {
                        break serde_json::from_slice::<Value>(&bytes[at + 4..at + 4 + size])
                            .unwrap();
                    }
                }
            };
            assert_eq!(request["tools"], json!([]));
            assert_eq!(request["store"], false);
            writeln!(
                OpenOptions::new().append(true).open(&log).unwrap(),
                "{request}"
            )
            .unwrap();
            let body=json!({"id":"mock-summary","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let mut f = Fixture::new().await;
    f.compactor = Compactor::new(
        f.db.clone(),
        Arc::new(Config {
            connections: std::collections::BTreeMap::from([("p".into(), connection)]),
            ..Default::default()
        }),
    );
    (f, server)
}
async fn edit(f: &Fixture, a: &ContextEditArgs) -> Result<ContextEditResult, ApiError> {
    crate::test_watchdog::watchdog(
        "src/compaction/edit/bounded_tests.rs: edit",
        f.compactor.edit("t", a, CancellationToken::new()),
    )
    .await
    .expect("summary fixture watchdog")
}
async fn finish(f: Fixture, server: tokio::task::JoinHandle<()>) {
    crate::test_watchdog::watchdog("src/compaction/edit/bounded_tests.rs: finish", server)
        .await
        .expect("mock server watchdog")
        .unwrap();
    f.close().await;
}

#[tokio::test]
async fn one_request_summary_preserves_latest_state_and_adopts_exact_selected_sources() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let final_text = "# Purpose\nFinish release.\n# Constraints\nMock only.\n# Current state\nLatest: published. Decision: approve after checks. Unresolved: none. Evidence n3.";
    let (f, server) = setup(vec![final_text.into()], &log).await;
    for i in 0..4 {
        f.text(
            &format!("n{i}"),
            &format!(
                "state {i}: {} {}",
                if i == 3 {
                    "Latest: published. Decision: approve after checks. Unresolved: none."
                } else {
                    "Earlier planned."
                },
                "x".repeat(62_000)
            ),
        )
        .await;
    }
    let before = lines(&log);
    assert_eq!(before, 0);
    let result = edit(&f, &args("n3")).await.unwrap();
    assert_eq!(result.compact.model_requests, Some(1));
    assert_eq!(lines(&log) - before, 1);
    assert!(result.after_characters < result.before_characters);
    assert_eq!(result.compact.source_nodes, 4);
    assert_eq!(result.compact.original_node_ids, ["n0", "n1", "n2", "n3"]);
    assert!(result.compact.summary_characters.unwrap() <= result.compact.summary_limit.unwrap());
    let path = f.path().await;
    assert_eq!(path.len(), 1);
    let id = path[0].clone();
    let payload =
        f.db.call(move |db| {
            Ok(
                db.query_row("SELECT payload_json FROM node WHERE id=?1", [id], |r| {
                    r.get::<_, String>(0)
                })?,
            )
        })
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(value["item"]["content"][0]["text"], final_text);
    assert_eq!(value["summary_scope"], "selected_range");
    finish(f, server).await;
}

#[tokio::test]
async fn summary_retries_oversize_output_then_adopts_on_later_invocation() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let mut replies = vec!["あ".repeat(2001); 5];
    replies.push("valid concise summary".into());
    let (f, server) = setup(replies, &log).await;
    f.text("n0", &"source".repeat(5000)).await;
    let before = f.path().await;
    let mut a = args("n0");
    a.max_chars = Some(1); // The effective floor remains 2000.
    let e = edit(&f, &a).await.unwrap_err();
    let d = e.details.unwrap();
    assert_eq!(d["summary_limit"], 2000);
    assert_eq!(d["summary_characters"], 2001);
    assert_eq!(d["model_requests"], 5);
    assert_eq!(lines(&log), 5);
    assert_eq!(f.path().await, before);
    let result = edit(&f, &a).await.unwrap();
    assert!(result.compact.summary_node_id.is_some());
    assert_eq!(lines(&log), 6);
    assert_ne!(f.path().await, before);
    finish(f, server).await;
}

#[tokio::test]
async fn oversized_retry_is_bounded_across_operations() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let mut replies = vec!["あ".repeat(2001)];
    replies.extend(vec!["short changed selection".into(); 2]);
    let (f, server) = setup(replies, &log).await;
    f.text("n0", &"source".repeat(5000)).await;
    let before = f.path().await;
    let mut a = args("n0");
    a.max_chars = Some(1);
    assert!(edit(&f, &a).await.is_err());
    assert!(edit(&f, &a).await.is_err());
    let count = lines(&log);
    assert_eq!(count, 1);
    let error = edit(&f, &a).await.unwrap_err();
    assert!(error.message.contains("retry already used"));
    assert_eq!(lines(&log), count);
    assert_eq!(f.path().await, before);
    f.text("n1", &"changed source".repeat(500)).await;
    a.to_node_id = Some("n1".into());
    let result = edit(&f, &a).await.unwrap();
    assert!(result.compact.summary_node_id.is_some());
    assert_ne!(f.path().await, before);
    server.abort();
    f.close().await;
}

#[tokio::test]
async fn bounded_summary_override_and_unicode_character_count_and_nonshrinking_rejection() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (f, server) = setup(vec!["あ".repeat(2000)], &log).await;
    f.text("n0", &"source".repeat(10_000)).await;
    let mut a = args("n0");
    a.max_chars = Some(3000);
    let r = edit(&f, &a).await.unwrap();
    assert_eq!(r.compact.summary_characters, Some(2000));
    assert_eq!(r.compact.summary_limit, Some(3000));
    assert_eq!(lines(&log), 1);
    finish(f, server).await;
    let (f, server) = setup(vec!["too large for a tiny source".into()], &log).await;
    f.text("n0", "a").await;
    assert!(
        edit(&f, &args("n0"))
            .await
            .unwrap_err()
            .message
            .contains("does not shrink")
    );
    assert_eq!(f.path().await, ["n0"]);
    assert_eq!(lines(&log), 1);
    finish(f, server).await;
}

#[tokio::test]
async fn large_explicit_range_is_sent_as_one_summary_request() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (f, server) = setup(vec!["Final state; exact sources retained".into()], &log).await;
    for i in 0..65 {
        f.text(&format!("n{i}"), &"x".repeat(63_000)).await;
    }
    let result = edit(&f, &args("n64")).await.unwrap();
    assert_eq!(result.compact.model_requests, Some(1));
    assert_eq!(lines(&log), 1);
    let request: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
    let prompt = request["input"][1]["content"][0]["text"].as_str().unwrap();
    for i in [0, 32, 64] {
        assert!(prompt.contains(&format!("n{i}:")));
    }
    assert!(result.after_characters < result.before_characters);
    finish(f, server).await;
}
