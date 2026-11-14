//! Public terminal facts from deterministic localhost provider responses.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t10_responses_untyped_error_is_terminal_with_separate_status_provenance() {
    terminal_public_case("sse_terminal_responses").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t10_responses_embedded_429_over_http_200_does_not_become_rate_status() {
    terminal_public_case("sse_terminal_429_responses").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t10_chat_untyped_error_preserves_terminal_status_provenance() {
    terminal_public_case("sse_terminal_chat").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t11_responses_genuine_eof_retains_bounded_retry_vector() {
    terminal_public_case("sse_eof_responses").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t11_responses_genuine_eof_with_zero_retry_budget_posts_once() {
    terminal_public_case("sse_eof_r0_responses").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t11_chat_genuine_eof_with_zero_retry_budget_posts_once() {
    terminal_public_case("sse_eof_r0_chat").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t11_chat_genuine_eof_retains_bounded_retry_vector() {
    terminal_public_case("sse_eof_chat").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_provider_failure_has_one_public_outcome() {
    terminal_public_case("provider").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t5_context_terminal_has_one_public_outcome() {
    terminal_public_case("context").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_complete_http400_shrinks_to_minimum_public_outcome() {
    terminal_public_case("s2_minimum_http400").await;
    terminal_public_case("s2_minimum_http400_success").await;
    terminal_public_case("s2_ineligible_floor").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_complete_http413_and_422_are_input_refusal_controls() {
    terminal_public_case("s2_minimum_http413").await;
    terminal_public_case("s2_minimum_http422").await;
    terminal_public_case("s2_minimum_sse400").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_http_body_codes_are_not_stream_witnesses() {
    terminal_public_case("s2_minimum_http400_body500").await;
    terminal_public_case("s2_http500_body400").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_actual_auth_and_rate_status_veto_typed_context_halving() {
    for case in [
        "s2_veto_401_context",
        "s2_veto_403_context",
        "s2_veto_404_context",
        "s2_veto_429_context",
        "s2_veto_quota_context",
    ] {
        terminal_public_case(case).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_http500_body400_is_not_a_stream_witness() {
    terminal_public_case("s2_http500_body400").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_incomplete_numeric_http_refusals_never_halve_or_suppress() {
    terminal_public_case("s2_incomplete_http400").await;
    terminal_public_case("s2_truncated_http413").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_retry_after_header_quota_is_public_provider_veto() {
    terminal_public_case("s2_retry_after_429").await;
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
    let context = case == "context" || case.ends_with("_context");
    let s2_stream_minimum = matches!(case, "sse_terminal_responses" | "sse_terminal_chat");
    let s2_minimum = case.starts_with("s2_minimum_") || s2_stream_minimum;
    let s2_veto = case.starts_with("s2_veto_");
    let s2_ineligible = case == "s2_ineligible_floor";
    let s2_success = case == "s2_minimum_http400_success";
    let s2_incomplete = case.starts_with("s2_incomplete_") || case == "s2_truncated_http413";
    let s2_header_quota = case == "s2_retry_after_429";
    let sse_terminal = matches!(
        case,
        "sse_terminal_responses"
            | "sse_terminal_chat"
            | "sse_terminal_429_responses"
            | "s2_minimum_sse400"
    );
    let sse_eof = case.starts_with("sse_eof_");
    let zero_retry_eof = case.contains("_r0_");
    let stream_case = sse_terminal || sse_eof;
    let chat_api = case.ends_with("_chat");
    let retry_limit = if stream_case && !zero_retry_eof { 2 } else { 0 };
    let embedded_status = if case == "sse_terminal_429_responses" {
        429
    } else {
        400
    };
    let expected_summary_posts =
        if s2_incomplete || s2_header_quota || context && s2_veto || s2_ineligible {
            1
        } else if s2_success {
            2
        } else if context || s2_minimum || (sse_eof && !zero_retry_eof) {
            3
        } else {
            1
        };
    crate::test_watchdog::watchdog("public terminal compaction contract", async {
        let root = tempfile::tempdir().unwrap();
        let (url, mut receive, server) = proxy().await;
        let owner = if stream_case {
            proactive_owner_retries(root.path(), &url, "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n", retry_limit, chat_api).await
        } else {
            proactive_owner(root.path(), &url, "[compaction]\nauto_at_tokens=200\nsummary_max_chars=4000\n").await
        };
        let control = Control::new(&owner, root.path().into());
        let target = thread(&control).await;
        let first = |text: &str, input| if chat_api { chat_answer_with_usage(text, input) } else { answer_with_usage(text, input) };
        send(&control, &target, &"A".repeat(8192)).await;
        let initial = incoming(&control, &mut receive).await;
        initial.reply.send((200, first(&"C".repeat(8192), 100))).unwrap();
        settled(&control, &target).await;
        send(&control, &target, &"B".repeat(8192)).await;
        let initial = incoming(&control, &mut receive).await;
        initial.reply.send((200, first(&"D".repeat(8192), 200))).unwrap();
        settled(&control, &target).await;
        send(&control, &target, "trigger summary").await;
        let mut summary_posts = 0;
        let mut held_sse_release = None;
        loop {
            let mut next = incoming(&control, &mut receive).await;
            let automatic_summary = if chat_api {
                next.body.get("tools").and_then(Value::as_array).is_none_or(Vec::is_empty)
            } else {
                next.body["tools"] == json!([])
            };
            if automatic_summary {
                summary_posts += 1;
                if sse_terminal || sse_eof {
                    let raw = if sse_eof {
                        if chat_api {
                            String::from("data: {\"id\":\"eof\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":null}]}\n\n")
                        } else {
                            String::from("data: {\"type\":\"response.created\",\"response\":{\"id\":\"eof\",\"status\":\"in_progress\"}}\n\n")
                        }
                    } else {
                        format!("data: {{\"error\":{{\"code\":\"{embedded_status}\",\"message\":\"SECRET_PROVIDER_MESSAGE\"}}}}\n\n")
                    };
                    if !sse_eof {
                        held_sse_release = next.release.take();
                    }
                    next.reply.send((200,json!({"_test_sse_body":raw,"_test_hold_sse_open":!sse_eof}))).unwrap();
                    continue;
                }
                let code = if context { "context_length_exceeded" } else if s2_minimum { if case.ends_with("body500") { "500" } else { "invalid_request_error" } } else if case == "s2_http500_body400" { "400" } else { "SECRET_PROVIDER_CODE" };
                        let response = if case == "protocol" {
                    (200, json!({"status":"completed","output":"SECRET_PROVIDER_MESSAGE"}))
                } else if case == "s2_minimum_http400_success" && summary_posts == 2 {
                    (200, answer("summary"))
                } else {
                    let body = if case == "s2_veto_quota_context" {
                        json!({"error":{"message":"SECRET_PROVIDER_MESSAGE","code":"context_length_exceeded","type":"usage_limit_reached","resets_at":chrono::Utc::now().timestamp()+3600}})
                    } else if case == "s2_retry_after_429" {
                        json!({"_test_retry_after":true,"error":{"message":"SECRET_PROVIDER_MESSAGE"}})
                    } else if s2_incomplete {
                        json!({"_test_incomplete":if case == "s2_truncated_http413" {"bounded"} else {"cutoff"}})
                    } else {
                        json!({"error":{"message":"SECRET_PROVIDER_MESSAGE","code":code,"compaction_outcome":{"kind":"rejected","reason":"profile_changed"}}})
                    };
                    (match case { "context" | "s2_minimum_http400" | "s2_minimum_http400_success" | "s2_minimum_http400_body500" | "s2_incomplete_http400" => 400, "s2_minimum_http413" | "s2_truncated_http413" => 413, "s2_minimum_http422" => 422, "s2_http500_body400" | "provider500" => 500, "provider429" | "s2_veto_429_context" | "s2_veto_quota_context" | "s2_retry_after_429" => 429, "s2_veto_403_context" => 403, "s2_veto_404_context" => 404, _ => 401 }, body)
                };
                next.reply.send(response).unwrap();
                if case == "s2_veto_quota_context" || case == "s2_retry_after_429" {
                    break;
                }
            } else {
                if let Some(release) = held_sse_release.take() {
                    let _ = release.send(());
                }
                let response = if chat_api { chat_answer_with_usage("conversation continues", 10) } else { answer_with_usage("conversation continues", 10) };
                next.reply.send((200, response)).unwrap();
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
        control.shutdown().await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
        server.abort();
        let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
        assert_eq!(summary_posts, expected_summary_posts, "fixed original-source admission vector");
        eprintln!("paired pre-S1 vector case={case}: summary_posts={summary_posts}");
        let expected = match case { "context" => "context_limit_terminal", "sse_terminal_responses" | "sse_terminal_chat" | "s2_minimum_http400" | "s2_minimum_http400_body500" | "s2_minimum_http413" | "s2_minimum_http422" | "s2_minimum_sse400" => "rejected_at_minimum", "protocol" | "sse_eof_responses" | "sse_eof_chat" | "sse_eof_r0_responses" | "sse_eof_r0_chat" => "unclassified", _ => "provider_error" };
        let items = list["items"].as_array().unwrap();
        if s2_success {
            assert!(failure_slots.is_empty(), "successful later trial must not leave provider or deterministic memory from an earlier HTTP 400");
            let completed = items.iter().find(|v| v["kind"] == "automatic_reduction").expect("automatic reduction outcome");
            assert_eq!(completed["state"], "adopted");
            assert_eq!(completed["compaction_outcome"], Value::Null);
            assert_eq!(completed["metrics"]["summary_usage"]["requests"], 2);
            return;
        }
        let item = items.iter().find(|v| v["kind"] == "automatic_reduction").expect("real automatic attempt must remain listed");
        let fact = &item["compaction_outcome"];
        assert_eq!(fact["outcome"]["kind"], "failed", "missing classified terminal public fact: {list:#}");
        let memory_kind = match (
            fact["outcome"]["reason"].as_str(),
            fact["outcome"]["origin"].as_str(),
        ) {
            (Some("context_limit_terminal" | "rejected_at_minimum"), _) => "deterministic_e",
            (Some("provider_error"), Some("provider_response")) => "provider_scope",
            _ => "none",
        };
        if case == "context" || case.starts_with("s2_") {
            assert_eq!(
                failure_slots.len(),
                usize::from(memory_kind != "none"),
                "slot presence must follow the terminal outcome's typed failure class: {case}, {fact:#?}"
            );
        }
        if s2_incomplete {
            assert_ne!(fact["outcome"]["reason"], "rejected_at_minimum");
            assert!(fact["outcome"].get("final_group_count").is_none());
            assert!(fact["outcome"].get("final_source_nodes").is_none());
            if case == "s2_truncated_http413" {
                assert_eq!(memory_kind, "none", "truncated response is unclassified, not provider-response evidence: {fact:#?}");
                assert!(failure_slots.is_empty(), "truncated response cannot create E or provider scope memory");
                assert!(fact["outcome"].get("http_status").is_none());
            } else {
                assert_eq!(memory_kind, "provider_scope", "complete HTTP 400 refusal is terminal ProviderResponse, but not deterministic E: {fact:#?}");
                assert_eq!(fact["outcome"]["http_status"], 400);
                assert!(fact["outcome"].get("provider_status").is_none());
            }
        } else if s2_header_quota {
            assert_eq!(fact["outcome"]["reason"], "provider_error");
            assert_eq!(fact["outcome"]["origin"], "provider_response");
            assert_eq!(fact["outcome"]["http_status"], 429);
            assert!(fact["outcome"].get("final_group_count").is_none());
            assert!(fact["outcome"].get("provider_status").is_none());
        } else {
            assert_eq!(fact["outcome"]["reason"], expected);
        }
        assert_eq!(fact["phase"], "closed_summary");
        if !s2_incomplete {
            assert_eq!(fact["outcome"]["origin"], if expected == "unclassified" { "protocol" } else { "provider_response" });
        }
        if case == "s2_http500_body400" {
            assert_eq!(fact["outcome"]["http_status"], 500);
            assert!(fact["outcome"].get("provider_status").is_none());
            assert!(fact["outcome"].get("provider_status_source").is_none());
        } else if case == "s2_minimum_sse400" {
            assert_eq!(fact["outcome"]["http_status"], 200);
            assert_eq!(fact["outcome"]["provider_status"], 400);
            assert_eq!(fact["outcome"]["provider_status_source"], "stream_error_code");
        } else if sse_terminal {
            assert_eq!(fact["outcome"]["http_status"], 200);
            assert_eq!(fact["outcome"]["provider_status"], embedded_status);
            assert_eq!(fact["outcome"]["provider_status_source"], "stream_error_code");
        } else if case == "protocol" || sse_eof {
            assert!(fact["outcome"].get("http_status").is_none());
            assert!(fact["outcome"].get("provider_status").is_none());
            assert!(fact["outcome"].get("provider_status_source").is_none());
        } else if case == "s2_truncated_http413" {
            assert!(fact["outcome"].get("http_status").is_none());
        } else {
            assert_eq!(fact["outcome"]["http_status"], match case { "context" | "s2_minimum_http400" | "s2_minimum_http400_body500" | "s2_incomplete_http400" => 400, "s2_minimum_http413" => 413, "s2_minimum_http422" => 422, "provider429" | "s2_veto_429_context" | "s2_veto_quota_context" => 429, "provider500" => 500, "s2_veto_403_context" => 403, "s2_veto_404_context" => 404, "s2_truncated_http413" => 413, "s2_retry_after_429" => 429, _ => 401 });
        }
        assert!(fact.to_string().len() <= 1024);
        assert!(fact["outcome"].get("provider_code").is_none());
        assert!(fact["outcome"].get("message").is_none());
        assert_eq!(item["metrics"]["summary_usage"]["requests"], summary_posts);
        assert_eq!(item["metrics"]["model_requests"], summary_posts);
        assert_eq!(item["metrics"]["summary_usage"]["input_tokens"]["unknown_count"], summary_posts);
        assert_eq!(item["metrics"]["summary_usage"]["input_tokens"]["known_sum"], Value::Null);
        if context && !s2_veto || s2_minimum {
            assert_eq!(fact["outcome"]["final_group_count"], 1);
            assert!(fact["outcome"]["final_source_nodes"].as_u64().is_some_and(|n| n > 0));
        }
        if s2_minimum && case != "s2_minimum_sse400" && !s2_stream_minimum {
            assert!(fact["outcome"].get("provider_status").is_none());
            assert!(fact["outcome"].get("provider_status_source").is_none());
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
            crate::compaction::attempt_begin_tx(
                &tx,
                &attempt,
                &attempt,
                REASON,
                crate::compaction::ABoundaryObservation::Observe,
                &json!({}),
                &Value::Null,
            )?;
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
