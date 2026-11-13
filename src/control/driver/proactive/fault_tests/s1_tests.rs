//! Public terminal facts from deterministic localhost provider responses.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_provider_failure_has_one_public_outcome() {
    terminal_public_case("provider").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_context_terminal_has_one_public_outcome() {
    terminal_public_case("context").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t8_protocol_failure_is_unclassified_not_deterministic() {
    terminal_public_case("protocol").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_provider_429_preserves_zero_retry_budget() {
    terminal_public_case("provider429").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_provider_500_preserves_zero_retry_budget() {
    terminal_public_case("provider500").await;
}

async fn terminal_public_case(case: &str) {
    let context = case == "context";
    crate::test_watchdog::watchdog("S1 public terminal contract", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        send(&control, &target, &"A".repeat(8192)).await;
        incoming(&control, &mut receive).await.reply.send((200, answer_with_usage(&"C".repeat(8192), 100))).unwrap();
        settled(&control, &target).await;
        send(&control, &target, &"B".repeat(8192)).await;
        incoming(&control, &mut receive).await.reply.send((200, answer_with_usage(&"D".repeat(8192), 200))).unwrap();
        settled(&control, &target).await;
        send(&control, &target, "trigger summary").await;
        let mut summary_posts = 0;
        loop {
            let next = incoming(&control, &mut receive).await;
            if next.body["tools"] == json!([]) {
                summary_posts += 1;
                let code = if context { "context_length_exceeded" } else { "SECRET_PROVIDER_CODE" };
                let response = if case == "protocol" {
                    (200, json!({"status":"completed","output":"SECRET_PROVIDER_MESSAGE"}))
                } else {
                    (match case { "context" => 400, "provider429" => 429, "provider500" => 500, _ => 401 }, json!({"error":{"message":"SECRET_PROVIDER_MESSAGE","code":code,"compaction_outcome":{"kind":"rejected","reason":"profile_changed"}}}))
                };
                next.reply.send(response).unwrap();
            } else {
                next.reply.send((200, answer_with_usage("conversation continues", 10))).unwrap();
                break;
            }
        }
        settled(&control, &target).await;
        let page = request(&control, Command::Status(StatusArgs { thread_id: Some(target.clone()), all: true, limit: 200, ..Default::default() })).await;
        let list = request(&control, Command::CompactList(CompactListArgs { thread_id: Some(target.clone()), since: None, until: None, limit: 20 })).await;
        let details = request(&control, Command::ThreadGet(ThreadTarget {thread_id: Some(target.clone())})).await;
        let scope = target.clone();
        let failure_slots = control.store.database.call(move |db| {
            let mut statement = db.prepare("SELECT key,value FROM app_meta WHERE key IN (?1,?2) ORDER BY key")?;
            let rows = statement.query_map(params![format!("compact.deterministic_range_failure.closed.{scope}"),format!("compact.deterministic_range_failure.active.{scope}")], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap();
        assert_eq!(failure_slots.len(), if context {1} else {0}, "source-owned deterministic predicate must preserve original two-slot truth set");
        control.shutdown().await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
        server.abort();
        let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
        assert_eq!(summary_posts, if context {3} else {1}, "fixed original-source admission vector");
        eprintln!("paired pre-S1 vector case={case}: summary_posts={summary_posts}");
        let expected = match case { "context" => "context_limit_terminal", "protocol" => "unclassified", _ => "provider_error" };
        let items = list["items"].as_array().unwrap();
        let item = items.iter().find(|v| v["kind"] == "automatic_reduction").expect("real automatic attempt must remain listed");
        let fact = &item["compaction_outcome"];
        assert_eq!(fact["outcome"]["kind"], "failed", "missing classified terminal public fact: {list:#}");
        assert_eq!(fact["outcome"]["reason"], expected);
        assert_eq!(fact["phase"], "closed_summary");
        assert_eq!(fact["outcome"]["origin"], if case == "protocol" { "protocol" } else { "provider_response" });
        if case == "protocol" {
            assert!(fact["outcome"].get("http_status").is_none());
        } else {
            assert_eq!(fact["outcome"]["http_status"], match case { "context" => 400, "provider429" => 429, "provider500" => 500, _ => 401 });
        }
        assert!(fact.to_string().len() <= 1024);
        assert!(fact["outcome"].get("provider_code").is_none());
        assert!(fact["outcome"].get("message").is_none());
        assert_eq!(item["metrics"]["summary_usage"]["requests"], summary_posts);
        assert_eq!(item["metrics"]["model_requests"], summary_posts);
        assert_eq!(item["metrics"]["summary_usage"]["input_tokens"]["unknown_count"], summary_posts);
        assert_eq!(item["metrics"]["summary_usage"]["input_tokens"]["known_sum"], Value::Null);
        if context {
            assert_eq!(fact["outcome"]["final_group_count"], 1);
            assert!(fact["outcome"]["final_source_nodes"].as_u64().is_some_and(|n| n > 0));
        }
        let status_item = page["items"].as_array().unwrap().iter().find(|v| v["id"] == item["attempt_id"]).unwrap();
        assert_eq!(status_item["compaction_outcome"], *fact);
        assert_eq!(details["last_compaction"]["compaction_outcome"], *fact);
        let outcomes: Vec<Value> = log.lines().filter_map(|s| serde_json::from_str::<Value>(s).ok()).filter(|v| v["event"] == "compaction_outcome").collect();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0]["compaction_outcome"], *fact);
        for value in [&page, &list, &details, &outcomes[0]] {
            let text = value.to_string();
            assert!(!text.contains("SECRET_PROVIDER_CODE"));
            assert!(!text.contains("SECRET_PROVIDER_MESSAGE"));
        }
        eprintln!("paired baseline context={context}: summary_posts={summary_posts}");
        assert!(summary_posts > 0);
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_canonical_guard_causes_share_one_public_projection_without_requests() {
    use crate::compaction::discard::{Outcome, RejectionReason as R};
    use crate::status::observation::{self, Key, Kind, TextBudget};
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(root.path(), &url, "").await;
    let control = Control::new(&owner, root.path().into());
    let target = thread(&control).await;
    let scope = target.clone();
    control.store.database.call(move |db| {
        db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('guard-run',?1,'completed','2026-11-01T00:00:00Z')", [&scope])?;
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('guard-turn',?1,'guard-run','completed','2026-11-01T00:00:00Z')", [&scope])?;
        Ok(())
    }).await.unwrap();
    let reasons = [
        R::Cancelled,
        R::ThreadUnavailable,
        R::SelectionChanged,
        R::ProfileChanged,
        R::SummarySettingsChanged,
        R::EpochChanged,
        R::RunStarted,
        R::RunStopped,
        R::TurnStarted,
        R::TurnChanged,
        R::NonappendGenerationChanged,
        R::SelectedPrefixChanged,
        R::CurrentTailChanged,
        R::SourceChanged,
        R::SourcePruned,
    ];
    for (index, reason) in reasons.into_iter().enumerate() {
        let id = format!("guard-attempt-{index:02}");
        let automatic = Automatic {
            attempt: id.clone(),
            token: id.clone(),
            thread: target.clone(),
            turn: "guard-turn".into(),
        };
        let attempt = id.clone();
        control.store.database.call(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES(?1,'guard-turn',0,0,'fixture','fixture',0,'completed','{}','2026-11-01T00:00:00Z')", [&attempt])?;
            crate::compaction::attempt_begin_tx(&tx, &attempt, &attempt, REASON, &json!({}), &Value::Null)?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        // Explicit owned-cause injection tests the common observation boundary;
        // real guard branch/precedence coverage lives in compaction guard tests.
        let mut error = ApiError::new(ErrorCode::InvalidState, "unchanged guard error");
        error.compaction_cause = Some(Box::new(Outcome::Rejected { reason }));
        let result = automatic
            .observed::<()>(&control, "closed_summary", async { Err(error.clone()) })
            .await
            .unwrap_err();
        assert_eq!(result, error);
        let page = request(
            &control,
            Command::Status(StatusArgs {
                thread_id: Some(target.clone()),
                all: true,
                limit: 200,
                ..Default::default()
            }),
        )
        .await;
        let list = request(
            &control,
            Command::CompactList(CompactListArgs {
                thread_id: Some(target.clone()),
                since: None,
                until: None,
                limit: 20,
            }),
        )
        .await;
        let details = request(
            &control,
            Command::ThreadGet(ThreadTarget {
                thread_id: Some(target.clone()),
            }),
        )
        .await;
        let point_id = id.clone();
        let point = control
            .store
            .database
            .call(move |db| {
                Ok(observation::prepare(observation::read_status_tx(
                    db,
                    &[Key {
                        kind: Kind::Attempt,
                        id: point_id,
                    }],
                    &mut TextBudget::default(),
                )?))
            })
            .await
            .unwrap();
        let item = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id)
            .unwrap();
        let fact = &item["compaction_outcome"];
        assert_eq!(fact["outcome"], json!({"kind":"rejected","reason":reason}));
        assert_eq!(point["items"][0]["current"]["compaction_outcome"], *fact);
        let listed = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["attempt_id"] == id)
            .unwrap();
        assert_eq!(listed["compaction_outcome"], *fact);
        // Equal timestamps are ordered by the fixed attempt IDs, newest first.
        assert_eq!(details["last_compaction"]["compaction_outcome"], *fact);
        assert!(fact.to_string().len() <= 1024);
    }
    let scope = target.clone();
    control.store.database.call(move |db| {
        assert_eq!(db.query_row("SELECT reasoning_epoch FROM thread WHERE id=?1", [&scope], |r| r.get::<_, i64>(0))?, 0);
        assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE thread_id=?1", [&scope], |r| r.get::<_, i64>(0))?, 0);
        assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'compact.deterministic_range_failure.%'", [], |r| r.get::<_, i64>(0))?, 0);
        Ok(())
    }).await.unwrap();
    assert!(
        receive.try_recv().is_err(),
        "observation never starts a model request"
    );
    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
    server.abort();
    let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
    let outcomes: Vec<Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["event"] == "compaction_outcome")
        .collect();
    assert_eq!(outcomes.len(), reasons.len());
    for (event, reason) in outcomes.iter().zip(reasons) {
        assert_eq!(
            event["compaction_outcome"]["outcome"],
            json!({"kind":"rejected","reason":reason})
        );
    }
    assert!(!log.contains("compaction_discard"));
}
