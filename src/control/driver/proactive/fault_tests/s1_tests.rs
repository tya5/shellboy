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

async fn terminal_public_case(case: &str) {
    let context = case == "context";
    crate::test_watchdog::watchdog("S1 public terminal contract", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n").await;
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        send(&control, &target, "synthetic source one").await;
        incoming(&control, &mut receive).await.reply.send((200, answer_with_usage("first answer", 100))).unwrap();
        settled(&control, &target).await;
        send(&control, &target, "synthetic source two").await;
        incoming(&control, &mut receive).await.reply.send((200, answer_with_usage("second answer", 200))).unwrap();
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
                    (if context {400} else {401}, json!({"error":{"message":"SECRET_PROVIDER_MESSAGE","code":code,"compaction_outcome":{"kind":"rejected","reason":"profile_changed"}}}))
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
        control.shutdown().await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
        server.abort();
        let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
        eprintln!("paired pre-S1 vector case={case}: summary_posts={summary_posts}");
        let expected = match case { "context" => "context_limit_terminal", "protocol" => "unclassified", _ => "provider_error" };
        let items = list["items"].as_array().unwrap();
        let item = items.iter().find(|v| v["kind"] == "automatic_reduction").expect("real automatic attempt must remain listed");
        let fact = &item["compaction_outcome"];
        assert_eq!(fact["outcome"]["kind"], "failed", "missing classified terminal public fact: {list:#}");
        assert_eq!(fact["outcome"]["reason"], expected);
        assert_eq!(fact["phase"], "closed_summary");
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
