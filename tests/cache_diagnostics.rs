//! #142 part 1: loopback-only request/adoption/inspection facts (JSON and SSE).
use super::request_inspect::{incoming, options, proxy_mode, request, settled, sha256, tool};
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    config::{Config, PathEnvironment},
    control::Control,
    protocol::*,
    request_inspect::inspect,
};
use std::sync::Arc;

async fn create(control: &Arc<Control>) -> String {
    request(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn send(control: &Arc<Control>, thread: &str) {
    request(
        control,
        Command::MessageSend(MessageSendArgs {
            resume_recovery: false,

            human: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.into()),
            text: "synthetic cache diagnostic fixture".into(),
        }),
    )
    .await;
}
fn answer(id: Option<&str>) -> Value {
    let mut value = json!({"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"fixture"}]}]});
    if let Some(id) = id {
        value["id"] = json!(id);
    }
    value
}
fn comparison(raw: &[u8]) -> Value {
    serde_json::from_slice::<Value>(raw).unwrap()["prompt_cache_options"]["comparison_response_id"]
        .clone()
}
async fn attempts(control: &Arc<Control>, thread: &str) -> Vec<String> {
    let thread = thread.to_owned();
    control.store.database.call(move|db| Ok(db.prepare("SELECT a.id FROM model_attempt a JOIN turn u ON u.id=a.turn_id WHERE u.thread_id=?1 ORDER BY a.rowid")?.query_map([thread],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)).await.unwrap()
}
async fn observed(control: &Arc<Control>, attempt: &str) -> Value {
    request(
        control,
        Command::UsageGet(UsageGetArgs {
            attempt_id: Some(attempt.into()),
            ..Default::default()
        }),
    )
    .await
}
async fn scenario(sse: bool) {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy_mode(sse).await;
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    let text = format!(
        "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries=0\nprompt_cache_diagnostics=true\n"
    );
    std::fs::write(&config, &text).unwrap();
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let thread = create(&control).await;
    send(&control, &thread).await;
    let diagnostics = [
        json!({"type":"cache_miss","reason":"model_changed","cache_missed_tokens":0}),
        Value::Null,
        json!({"type":"future_type","reason":"future_reason","cache_missed_tokens":7}),
    ];
    let mut sent = Vec::new();
    for (i, mut reply) in [
        tool("one", "echo one"),
        tool("two", "echo two"),
        answer(Some("r3")),
    ]
    .into_iter()
    .enumerate()
    {
        reply["id"] = json!(format!("r{}", i + 1));
        reply["usage"] = json!({"input_tokens":100,"input_tokens_details":{"cached_tokens":75},"output_tokens":10,"total_tokens":110});
        if !diagnostics[i].is_null() {
            reply["prompt_cache_diagnostics"] = diagnostics[i].clone();
            reply["prompt_cache_diagnostics"]["ignored"] = json!("x".repeat(20_000));
        }
        let next = incoming(&mut receive).await;
        assert_eq!(
            comparison(&next.raw),
            if i == 0 {
                Value::Null
            } else {
                json!(format!("r{i}"))
            }
        );
        sent.push(next.raw);
        next.reply.send(reply).unwrap();
    }
    settled(&control, &thread).await;
    let ids = attempts(&control, &thread).await;
    assert_eq!(ids.len(), 3);
    let loaded = Config::load_with_overrides(
        Some(&config),
        Some(&data),
        root.path(),
        &PathEnvironment::current(),
    )
    .unwrap();
    for (i, attempt) in ids.iter().enumerate() {
        let usage = observed(&control, attempt).await;
        assert_eq!(usage["attempt_id"], attempt.as_str());
        assert_eq!(usage["usage"]["input_tokens"], 100);
        assert_eq!(usage["prompt_cache_diagnostics"], diagnostics[i]);
        let rebuilt = inspect(&loaded, &options(&thread, Some(attempt), None, true)).unwrap();
        assert_eq!(rebuilt["unknown"], json!([]), "{rebuilt}");
        assert_eq!(rebuilt["total"]["sha256"], sha256(&sent[i]));
        assert_eq!(rebuilt["saved"]["prompt_cache_diagnostics"], diagnostics[i]);
        assert_eq!(
            rebuilt["body"]["prompt_cache_options"]["comparison_response_id"],
            comparison(&sent[i])
        );
    }
    // Inspection uses the admitted comparison, not the Thread's now-current r3.
    assert_eq!(
        inspect(&loaded, &options(&thread, None, None, true)).unwrap()["body"]["prompt_cache_options"]
            ["comparison_response_id"],
        "r3"
    );
    std::fs::write(
        &config,
        text.replace(
            "prompt_cache_diagnostics=true",
            "prompt_cache_diagnostics=false",
        ),
    )
    .unwrap();
    let changed = Config::load_with_overrides(
        Some(&config),
        Some(&data),
        root.path(),
        &PathEnvironment::current(),
    )
    .unwrap();
    let unknown = inspect(&changed, &options(&thread, Some(&ids[1]), None, true)).unwrap();
    assert!(
        unknown["unknown"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("cache diagnostics"))
    );
    std::fs::write(&config, &text).unwrap();

    // Cross-Turn reuse; a successfully adopted response with no ID clears it.
    send(&control, &thread).await;
    let next = incoming(&mut receive).await;
    assert_eq!(comparison(&next.raw), "r3");
    next.reply.send(answer(None)).unwrap();
    settled(&control, &thread).await;
    send(&control, &thread).await;
    let next = incoming(&mut receive).await;
    assert!(comparison(&next.raw).is_null());
    next.reply.send(answer(Some("last-good"))).unwrap();
    settled(&control, &thread).await;

    // Incomplete/failed terminal facts are recorded, never adopted as identity.
    send(&control, &thread).await;
    let next = incoming(&mut receive).await;
    assert_eq!(comparison(&next.raw), "last-good");
    let mut bad = answer(Some("not-adopted"));
    bad["status"] = json!("incomplete");
    bad["prompt_cache_diagnostics"] =
        json!({"type":"cache_miss","reason":"fixture_failure","cache_missed_tokens":3});
    next.reply.send(bad).unwrap();
    settled(&control, &thread).await;
    let all = attempts(&control, &thread).await;
    let failure = observed(&control, all.last().unwrap()).await;
    assert_ne!(failure["state"], "completed");
    assert_eq!(
        failure["prompt_cache_diagnostics"]["reason"],
        "fixture_failure"
    );
    assert_eq!(
        inspect(&loaded, &options(&thread, None, None, true)).unwrap()["body"]["prompt_cache_options"]
            ["comparison_response_id"],
        "last-good"
    );

    // Neither another Thread nor a fork inherits an adoption on this Thread.
    let other = create(&control).await;
    send(&control, &other).await;
    let next = incoming(&mut receive).await;
    assert!(comparison(&next.raw).is_null());
    next.reply.send(answer(Some("other-good"))).unwrap();
    settled(&control, &other).await;
    let target = other.clone();
    let node: String = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT answer_node_id FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",
                [target],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let fork = request(
        &control,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: other.clone(),
            node_id: node,
            cwd: None,
        }),
    )
    .await["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&control, &fork).await;
    let next = incoming(&mut receive).await;
    assert!(comparison(&next.raw).is_null());
    next.reply.send(answer(Some("fork-good"))).unwrap();
    settled(&control, &fork).await;

    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
    // The adopted ID is durable, not owner-process or Turn-local state.
    let owner = DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    send(&control, &other).await;
    let next = incoming(&mut receive).await;
    assert_eq!(comparison(&next.raw), "other-good");
    next.reply.send(answer(Some("after-restart"))).unwrap();
    settled(&control, &other).await;
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_cache_diagnostics_follow_adoption_and_are_inspectable() {
    crate::test_watchdog::watchdog("JSON cache diagnostics", scenario(false))
        .await
        .expect("fixture completes");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_cache_diagnostics_follow_adoption_and_are_inspectable() {
    crate::test_watchdog::watchdog("SSE cache diagnostics", scenario(true))
        .await
        .expect("fixture completes");
}
