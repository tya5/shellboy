//! Loopback-only summary fixtures. Timeouts are watchdogs, never timing assertions.
use super::tests::Fixture;
use super::*;
use std::{fs::OpenOptions, io::Write, path::Path};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Duration, timeout},
};

fn args(to: &str) -> ContextEditArgs {
    ContextEditArgs {
        thread_id: Some("t".into()),
        action: ContextEditAction::Summary,
        from_node_id: Some("n0".into()),
        to_node_id: Some(to.into()),
        max_chars: None,
        max_percent: None,
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
    timeout(
        Duration::from_secs(60),
        f.compactor.edit("t", a, CancellationToken::new()),
    )
    .await
    .expect("summary fixture watchdog")
}
async fn finish(f: Fixture, server: tokio::task::JoinHandle<()>) {
    timeout(Duration::from_secs(60), server)
        .await
        .expect("mock server watchdog")
        .unwrap();
    f.close().await;
}

#[tokio::test]
async fn bounded_summary_hierarchical_reduce_keeps_one_latest_state_and_logs_exact_requests() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let final_text = "# Purpose\nFinish release.\n# Constraints\nMock only.\n# Current state\nLatest: published. Decision: approve after checks. Unresolved: none. Evidence n3; earlier planned state superseded.";
    let mut responses: Vec<_> = (0..4)
        .map(|i| {
            if i == 3 {
                "Latest: published. Evidence n3. ".repeat(1400)
            } else {
                format!("Earlier planned; evidence n{i}. ").repeat(1400)
            }
        })
        .collect();
    responses.extend(vec![
        "Intermediate; latest confirmed published, evidence n3."
            .into();
        3
    ]);
    responses.push(final_text.into());
    let (f, server) = setup(responses, &log).await;
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
    let before_lines = lines(&log);
    assert_eq!(before_lines, 0);
    let r = edit(&f, &args("n3")).await.unwrap();
    assert_eq!(r.compact.model_requests, Some(8));
    assert_eq!(lines(&log) - before_lines, 8);
    assert!(r.after_characters < r.before_characters);
    assert_eq!(r.compact.source_nodes, 4);
    assert!(r.compact.summary_characters.unwrap() <= r.compact.summary_limit.unwrap());
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
    let p: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(p["item"]["content"][0]["text"], final_text);
    assert_eq!(final_text.matches("# Purpose").count(), 1);
    assert_eq!(final_text.matches("# Constraints").count(), 1);
    let requests: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(
        requests[4]["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("latest confirmed state")
    );
    assert!(
        requests[4]["input"][1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("n0")
    );
    for req in &requests[4..] {
        assert!(
            req["input"][1]["content"][0]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= SEGMENT_CHARS
        );
    }
    finish(f, server).await;
}

#[tokio::test]
async fn bounded_summary_retries_oversize_then_rejects_without_adopting_and_reuses_audit() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (f, server) = setup(vec!["あ".repeat(2001); 5], &log).await;
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
    let e = edit(&f, &a).await.unwrap_err();
    assert_eq!(e.details.unwrap()["model_requests"], 0);
    assert_eq!(
        lines(&log),
        5,
        "cached failed-size attempt must not call the mock again"
    );
    assert_eq!(f.path().await, before);
    finish(f, server).await;
}

async fn cache_group(f: &Fixture, index: usize, text: &str) {
    let snapshot = f.compactor.snapshot("t", true).await.unwrap();
    let sources = snapshot.groups[index].sources.clone();
    let cache_key = CompactionKeys::summary_part(&summary_key(&snapshot, &sources));
    let raw = json!({"id":"cached","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}]});
    CachedPart {
        text: text.into(),
        sources: sources.iter().map(|s| s.id.clone()).collect(),
        cache_key: Some(cache_key.clone()),
        raw_response: Some(raw),
    }
    .save(&f.db, cache_key)
    .await
    .unwrap();
}
fn map_limit(f: &mut Fixture, map: usize, reduce: usize) {
    let mut config = (*f.compactor.config).clone();
    config.compaction.summary_map_max_requests = map;
    config.compaction.summary_reduce_max_requests = reduce;
    f.compactor = Compactor::new(f.db.clone(), Arc::new(config));
}
#[tokio::test]
async fn map_preflight_refuses_without_http_and_cached_groups_do_not_consume_budget() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (mut f, server) = setup(
        vec!["new n2".into(), "Final; references n0 n1 n2".into()],
        &log,
    )
    .await;
    for i in 0..3 {
        f.text(&format!("n{i}"), &"x".repeat(63_000)).await;
    }
    map_limit(&mut f, 1, 1);
    cache_group(&f, 0, "cached n0").await;
    let before = f.path().await;
    let e = edit(&f, &args("n2")).await.unwrap_err();
    let facts = e.details.unwrap();
    assert_eq!(facts["map_segments"], 3);
    assert_eq!(facts["uncached_map_segments"], 2);
    assert_eq!(facts["map_limit"], 1);
    assert_eq!(facts["model_requests"], 0);
    assert_eq!(lines(&log), 0);
    assert_eq!(f.path().await, before);
    cache_group(&f, 1, "cached n1").await;
    let r = edit(&f, &args("n2")).await.unwrap();
    assert_eq!(r.compact.reused_parts, 2);
    assert_eq!(r.compact.created_parts, 1);
    assert_eq!(r.compact.model_requests, Some(2));
    assert_eq!(lines(&log), 2);
    finish(f, server).await;
}
#[tokio::test]
async fn reduce_budget_is_independent_of_map_budget() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (mut f, server) = setup(vec!["a".repeat(2001); 2], &log).await;
    f.text("n0", &"x".repeat(30_000)).await;
    map_limit(&mut f, 1, 1);
    let mut a = args("n0");
    a.max_chars = Some(1);
    let e = edit(&f, &a).await.unwrap_err();
    let d = e.details.unwrap();
    assert_eq!(d["map_requests"], 1);
    assert_eq!(d["reduce_requests"], 1);
    assert_eq!(d["model_requests"], 2);
    assert_eq!(lines(&log), 2);
    assert_eq!(f.path().await, ["n0"]);
    finish(f, server).await;
}
#[tokio::test]
async fn map_cache_survives_output_limit_changes() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (f, server) = setup(vec!["a".repeat(2001); 5], &log).await;
    f.text("n0", &"x".repeat(30_000)).await;
    let mut a = args("n0");
    a.max_chars = Some(1);
    edit(&f, &a).await.unwrap_err();
    assert_eq!(lines(&log), 5);
    a.max_chars = Some(3000);
    a.max_percent = Some(100.0);
    let r = edit(&f, &a).await.unwrap();
    assert_eq!(r.compact.model_requests, Some(0));
    assert_eq!(r.compact.reused_parts, 1);
    assert_eq!(lines(&log), 5);
    finish(f, server).await;
}
#[tokio::test]
async fn automatic_second_stage_uses_reduction_limits_and_never_adopts_oversize_text() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (mut f, server) = setup(vec!["a".repeat(2001); 2], &log).await;
    f.text("n0", &"x".repeat(30_000)).await;
    map_limit(&mut f, 1, 1);
    let mut config = (*f.compactor.config).clone();
    config.compaction.summary_max_chars = Some(1);
    f.compactor = Compactor::new(f.db.clone(), Arc::new(config));
    let before = f.path().await;
    let snapshot = f.compactor.snapshot("t", true).await.unwrap();
    let e = timeout(
        Duration::from_secs(60),
        f.compactor
            .summarize_snapshot(snapshot, 0..1, CancellationToken::new()),
    )
    .await
    .expect("automatic stage watchdog")
    .unwrap_err();
    assert_eq!(e.details.unwrap()["reduce_requests"], 1);
    assert_eq!(f.path().await, before);
    assert_eq!(lines(&log), 2);
    finish(f, server).await;
}

#[tokio::test]
async fn bounded_summary_override_and_unicode_character_count_and_nonshrinking_rejection() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let (f, server) = setup(vec!["あ".repeat(2000)], &log).await;
    f.text("n0", &"source".repeat(10_000)).await;
    let mut a = args("n0");
    a.max_chars = Some(3000);
    a.max_percent = Some(4.0);
    let r = edit(&f, &a).await.unwrap();
    assert_eq!(r.compact.summary_characters, Some(2000));
    assert_eq!(
        r.compact.summary_limit,
        Some((r.compact.source_characters.unwrap() as f64 * 0.04).floor() as u64)
    );
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
async fn default_map_budget_allows_more_than_sixty_four_parts() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("requests.jsonl");
    let mut responses = vec!["part".into(); 65];
    responses.push("Final state; exact sources retained".into());
    let (f, server) = setup(responses, &log).await;
    for i in 0..65 {
        f.text(&format!("n{i}"), &"x".repeat(63_000)).await;
    }
    let result = edit(&f, &args("n64")).await.unwrap();
    assert_eq!(result.compact.model_requests, Some(66));
    assert_eq!(lines(&log), 66);
    assert_eq!(result.compact.created_parts, 65);
    assert!(result.after_characters < result.before_characters);
    finish(f, server).await;
}
