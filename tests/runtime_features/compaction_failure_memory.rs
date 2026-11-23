//! Public S2b provider-memory controls using the shared real-owner loopback fixture.
use super::*;

/// Repeated eligible reports in one real Run/Turn/generation/owner scope may
/// reuse one provider invocation, even when each request is a terminal veto.
#[tokio::test]
async fn provider_veto_is_one_invocation_per_real_scope() {
    let result = ax_growing_turn_summary_requests(auth_veto_reply, 0).await;
    assert_scope_trace(&result);
    assert_provider_scope_records(&result, 401, 1);
}

/// The fixture exposes actual owner instance, Run, Turn and generation through
/// the persisted model-attempt/Turn join. E's endpoint IDs are intentionally
/// not in public trace payloads; unchanged original history is checked here via
/// the captured path prefix, so the exact private E digest remains a fixture-only
/// identity proof gap rather than a fabricated/public DTO assertion.
pub(super) fn assert_scope_trace(result: &AxFixtureResult) {
    assert!(!result.owner_instance_id.is_empty());
    assert_eq!(
        result.report_owners.len(),
        17,
        "all 17 raw known-usage rows were captured before response"
    );
    let mut raw_ids = result
        .report_owners
        .iter()
        .map(|owner| owner.attempt_id.clone())
        .collect::<Vec<_>>();
    let mut admitted_ids = result.report_attempt_ids.clone();
    let mut unadmitted_ids = result.unadmitted_report_attempt_ids.clone();
    raw_ids.sort_unstable();
    assert!(
        raw_ids.windows(2).all(|pair| pair[0] != pair[1]),
        "each ordinary input has a distinct original attempt row"
    );
    admitted_ids.sort_unstable();
    unadmitted_ids.sort_unstable();
    assert_eq!(
        result.records.len(),
        admitted_ids.len(),
        "one terminal outcome per durable attempt_begin"
    );
    assert!(
        admitted_ids.len() >= 2,
        "nonvacuous provider-suppression coverage requires a later admitted report"
    );
    assert!(
        admitted_ids.iter().all(|id| !unadmitted_ids.contains(id)),
        "admission partition is disjoint"
    );
    let mut partition_ids = admitted_ids.clone();
    partition_ids.extend(unadmitted_ids);
    partition_ids.sort_unstable();
    assert_eq!(
        partition_ids, raw_ids,
        "every raw report is explicitly admitted or unadmitted after producer drain"
    );
    assert_eq!(
        result.report_scopes.len(),
        17,
        "all predeclared raw report rows are retained"
    );
    assert!(
        result
            .report_scopes
            .iter()
            .all(|scope| scope == &result.expected_scope),
        "raw report Run/Turn/generation changed: {:?}",
        result.report_scopes
    );
    let mut report_tokens = result
        .report_owners
        .iter()
        .map(|owner| owner.input_tokens)
        .collect::<Vec<_>>();
    report_tokens.sort_unstable();
    assert_eq!(
        report_tokens, result.expected_report_tokens,
        "all original known usage values 202..218 remain present"
    );
    assert!(
        result
            .report_owners
            .iter()
            .any(|owner| owner.input_tokens == 202 && admitted_ids.contains(&owner.attempt_id)),
        "first threshold-crossing report 202 must be an actual accepted duty"
    );
    assert!(
        result.final_path_ids.starts_with(&result.initial_path_ids),
        "the original path prefix changed before explicit compact/shutdown"
    );
}

/// Assert every actually admitted duty while keeping the predeclared raw-report
/// input set distinct. Suppression is tested only after the real first FINISH.
pub(super) fn assert_provider_scope_records(
    result: &AxFixtureResult,
    status: u16,
    admitted_requests: u64,
) {
    assert_eq!(
        result.summaries, admitted_requests as usize,
        "actual synthetic HTTP summary POST count: {:#?}",
        result.records
    );
    assert_eq!(
        result.records.len(),
        result.report_attempt_ids.len(),
        "all and only durable accepted duties have records"
    );
    assert!(
        result.records.len() >= 2,
        "failure-memory negative requires a later admitted suppression duty"
    );
    let admitted: Vec<_> = result
        .records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "provider_error")
        .collect();
    assert_eq!(
        admitted.len(),
        1,
        "one provider response outcome: {:#?}",
        result.records
    );
    let first_owner = result
        .report_owners
        .iter()
        .find(|owner| owner.input_tokens == 202)
        .expect("captured first known-usage report");
    let first_index = result
        .report_attempt_ids
        .iter()
        .position(|id| id == &first_owner.attempt_id)
        .expect("first 202 report was accepted");
    assert_eq!(
        admitted[0], &result.records[first_index],
        "the first admitted report owns the provider response"
    );
    assert_eq!(admitted[0]["trigger"]["input_tokens"], 202);
    let outcome = &admitted[0]["compaction_outcome"]["outcome"];
    assert_eq!(outcome["kind"], "failed");
    assert_eq!(outcome["origin"], "provider_response");
    assert_eq!(outcome["http_status"], status);
    for field in [
        "provider_status",
        "provider_status_source",
        "final_group_count",
        "final_source_nodes",
    ] {
        assert!(outcome.get(field).is_none(), "{field}: {outcome:#?}");
    }
    assert_eq!(
        admitted[0]["error"]["details"]["model_requests"],
        admitted_requests
    );
    assert_eq!(
        admitted[0]["error"]["details"]["summary_usage"]["requests"],
        admitted_requests
    );
    assert_eq!(
        result.initial_failure_event["attempt_id"],
        first_owner.attempt_id
    );
    assert_eq!(
        result.initial_failure_event["metrics"]["summary_usage"]["requests"],
        admitted_requests
    );

    let suppressed: Vec<_> = result
        .records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "retry_suppressed")
        .collect();
    assert_eq!(
        suppressed.len(),
        result.records.len() - 1,
        "every later admitted same-scope duty is suppressed: {:#?}",
        result.records
    );
    assert!(
        !suppressed.is_empty(),
        "a later accepted duty is required to prove same-scope suppression"
    );
    for record in suppressed {
        let outcome = &record["compaction_outcome"]["outcome"];
        assert_eq!(outcome["kind"], "failed");
        assert_eq!(outcome["origin"], "preparation");
        for field in [
            "http_status",
            "provider_status",
            "provider_status_source",
            "final_group_count",
            "final_source_nodes",
        ] {
            assert!(outcome.get(field).is_none(), "{field}: {outcome:#?}");
        }
        assert!(
            record["error"].get("details").is_none(),
            "suppression must omit request and usage facts: {record:#?}"
        );
    }
}

fn auth_veto_reply() -> (u16, Value) {
    (
        401,
        json!({"error":{"code":"invalid_request_error","message":"synthetic auth veto"}}),
    )
}

/// Every actual HTTP veto class outranks the numeric/refusal-looking body and
/// shares the one-invocation provider policy for the captured real scope.
#[tokio::test]
async fn auth_not_found_and_rate_vetoes_share_provider_scope_without_halving() {
    for (status, reply) in [
        (403, forbidden_reply as fn() -> (u16, Value)),
        (404, not_found_reply as fn() -> (u16, Value)),
        (429, rate_limit_reply as fn() -> (u16, Value)),
    ] {
        let result = ax_growing_turn_summary_requests(reply, 0).await;
        assert_eq!(
            result.summaries, 1,
            "HTTP {status}: one invocation in the real scope"
        );
        assert_scope_trace(&result);
        assert_provider_scope_records(&result, status, 1);
    }
}

fn forbidden_reply() -> (u16, Value) {
    (
        403,
        json!({"error":{"code":"invalid_request_error","message":"synthetic veto"}}),
    )
}
fn not_found_reply() -> (u16, Value) {
    (
        404,
        json!({"error":{"code":"invalid_request_error","message":"synthetic veto"}}),
    )
}
fn rate_limit_reply() -> (u16, Value) {
    (
        429,
        json!({"error":{"code":"invalid_request_error","message":"synthetic veto"}}),
    )
}

/// The memory unit is one admitted invocation, not one individual HTTP POST.
/// With two configured transient retries, the first report makes three POSTs;
/// the repeated same-scope report is suppressed without another invocation.
#[tokio::test]
async fn provider_scope_counts_invocation_not_transient_posts() {
    let result = one_provider_invocation(retryable_server_error_reply, 2).await;
    assert_eq!(
        result.summary_posts, 3,
        "one declared invocation makes its exact retry vector"
    );
    assert_eq!(
        result.scopes.len(),
        1,
        "one policy invocation owns one captured Run/Turn scope"
    );
    assert_eq!(
        result.bodies.len(),
        result.summary_posts,
        "each actual provider POST retains its request body"
    );
    assert!(
        result.bodies.iter().all(|body| body == &result.bodies[0]),
        "all retry request bodies are byte-equivalent JSON values"
    );
    assert_eq!(result.bodies[0]["tools"], json!([]));
    let input = result.bodies[0]["input"].to_string();
    assert!(
        input.contains("task 0"),
        "the predeclared older Turn is summarized: {input}"
    );
    assert!(
        !input.contains("task 1"),
        "the latest completed Turn stays outside R"
    );
    assert!(
        !input.contains("current task"),
        "the triggering request remains protected"
    );
    assert_eq!(result.record["trigger"]["input_tokens"], 150);
    assert_eq!(
        result.record["compaction_outcome"]["outcome"]["reason"],
        "provider_error"
    );
    assert_eq!(
        result.record["compaction_outcome"]["outcome"]["http_status"],
        500
    );
    assert_eq!(result.record["error"]["details"]["model_requests"], 3);
    assert_eq!(
        result.record["error"]["details"]["summary_usage"]["requests"],
        3
    );
}

fn retryable_server_error_reply() -> (u16, Value) {
    (
        500,
        json!({"error":{"code":"server_error","message":"synthetic transient failure"}}),
    )
}

struct ProviderInvocation {
    summary_posts: usize,
    scopes: Vec<(String, String)>,
    bodies: Vec<Value>,
    record: Value,
}

/// One source-proven eligible threshold report isolates request retries from
/// the asynchronous `next_request_pending` gate on later ordinary reports.
async fn one_provider_invocation(
    summary_reply: fn() -> (u16, Value),
    retries: u8,
) -> ProviderInvocation {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!("default_connection=\"fixture\"\ndefault_model=\"fixture\"\n[connections.fixture]\nbase_url={url:?}\nresponse_max_retries={retries}\nresponse_retry_max_delay_ms=10\nspill=false\n[compaction]\nauto_at_tokens=100\nsummary_max_chars=4000\n"),
    ).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let thread = thread(&control).await;
    let mut report = None;
    for (task, tokens) in [("task 0", 50), ("task 1", 150)] {
        let accepted = send_accepted(&control, &thread, task).await;
        let turn = accepted.turn_id.expect("warmup has its own accepted Turn");
        let next = incoming(&control, &mut receive).await;
        assert_eq!(next.kind(), ProviderRequestKind::Ordinary);
        assert_current_context_once(
            &next.body,
            &current_context(&control, &turn).await,
            "warmup ordinary request",
        );
        if task == "task 1" {
            report = Some(
                prepared_report_from_incoming(&control, &turn, 1, &next, tokens)
                    .await
                    .owner,
            );
        }
        next.reply
            .send((200, answer_with_usage("saved answer", tokens)))
            .unwrap();
        settled(&control, &thread).await;
    }
    let report = report.expect("task 1 supplies the exact threshold report");
    let accepted = send_accepted(&control, &thread, "current task").await;
    let trigger = TriggerOwner {
        run_id: accepted.run_id.expect("current task has an accepted Run"),
        turn_id: accepted.turn_id.expect("current task has an accepted Turn"),
    };
    let report_turns = [report.row_turn_id.clone()];
    let finish = automatic_attempts_finished_for_turns(&control, &report_turns, 1);
    let settle = settled(&control, &thread);
    tokio::pin!(finish, settle);
    let mut terminal = None;
    let mut foreground_replied = false;
    let mut foreground_settled = false;
    let mut invocation: Option<ReportBoundary> = None;
    let mut bodies = Vec::new();
    while terminal.is_none() || !foreground_settled {
        tokio::select! {
            attempts = &mut finish, if terminal.is_none() => terminal = Some(attempts),
            _ = &mut settle, if foreground_replied && !foreground_settled => {
                foreground_settled = true;
            }
            next = incoming(&control, &mut receive) => match next.kind() {
                ProviderRequestKind::Ordinary => {
                    assert!(!foreground_replied, "one ordinary request for the accepted current task");
                    assert_current_context_once(
                        &next.body,
                        &current_context(&control, &trigger.turn_id).await,
                        "current task ordinary request",
                    );
                    next.reply.send((200, answer_with_usage("last answer", 10))).unwrap();
                    foreground_replied = true;
                }
                ProviderRequestKind::Summary => {
                    // The request is still held: the provisional report and active
                    // duty must identify its owner even after the ordinary Turn settles.
                    let admitted = admitted_summary_boundary(
                        &control,
                        std::slice::from_ref(&report),
                        trigger.clone(),
                    ).await;
                    let key = format!("compact.automatic_active.{thread}");
                    let duty: Value = control.store.database.call(move |db| {
                        let saved: String = db.query_row(
                            "SELECT value FROM app_meta WHERE key=?1", [key], |row| row.get(0),
                        )?;
                        Ok(serde_json::from_str(&saved).expect("active duty is JSON"))
                    }).await.unwrap();
                    assert_eq!(duty["run_id"], trigger.run_id);
                    assert_eq!(duty["turn_id"], trigger.turn_id);
                    assert_eq!(duty["input_tokens"], report.input_tokens);
                    if let Some(first) = &invocation {
                        assert_eq!(admitted.report.attempt_id, first.report.attempt_id);
                        assert_eq!(admitted.gc_token, first.gc_token);
                    } else {
                        invocation = Some(admitted);
                    }
                    if let Some(first_body) = bodies.first() {
                        assert_eq!(&next.body, first_body, "retry preserves the exact original request body");
                    }
                    bodies.push(next.body);
                    assert!(bodies.len() <= retries as usize + 1, "unexpected extra Summary POST");
                    next.reply.send(summary_reply()).unwrap();
                }
            }
        }
    }
    let terminal = terminal.expect("exact durable FINISH was observed");
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].id, report.attempt_id);
    let invocation = invocation.expect("the exact Summary invocation reached the provider");
    let finished_events = proactive_events(&control, &thread).await;
    assert_eq!(
        finished_events.len(),
        1,
        "one predeclared eligible report still owns exactly one attempt"
    );
    assert_eq!(finished_events[0]["attempt_id"], terminal[0].id);
    let record = automatic_attempt_record(&control, report.attempt_id.clone()).await;
    assert_eq!(record["trigger"]["run_id"], trigger.run_id);
    let finished = invocation.finish(&terminal[0], &record);
    let scope = finished.boundary.trigger;
    let summary_posts = bodies.len();
    assert_eq!(
        summary_posts,
        retries as usize + 1,
        "the exact provider POST retry vector is preserved for the one captured scope"
    );
    assert!(matches!(
        receive.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
    ProviderInvocation {
        summary_posts,
        scopes: vec![(scope.run_id, scope.turn_id)],
        bodies,
        record,
    }
}

/// A real later Run and its own current Turn reopen provider-scoped inhibition.
/// The pure key test separately proves the Run component changes while E stays fixed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legal_new_run_reopens_provider_failure_memory() {
    let root = tempfile::tempdir().unwrap();
    let (url, mut receive, server) = proxy().await;
    let owner = proactive_owner(
        root.path(),
        &url,
        "[compaction]\nauto_at_tokens=100\nsummary_max_chars=4000\n",
    )
    .await;
    let control = Control::new(&owner, root.path().into());
    let target = thread(&control).await;

    let mut first_trace = proactive_third_turn_after_run(
        &control,
        &mut receive,
        &target,
        150,
        auth_veto_reply(),
        None,
    )
    .await;
    let first_finished =
        finish_proactive_trace(&control, &mut receive, &mut first_trace, &auth_veto_reply()).await;
    assert!(
        !first_finished.is_empty(),
        "first real Run has an exact served provider duty"
    );
    assert!(
        !first_trace.summary_scopes.is_empty(),
        "initial real Run admits provider summary requests"
    );
    let first_events = proactive_events(&control, &target).await;
    let first_attempt = first_events
        .iter()
        .find(|event| event["metrics"]["summary_usage"]["requests"] == 1)
        .expect("initial provider summary outcome")["attempt_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_attempt_for_query = first_attempt.clone();
    let first_scope = control
        .store
        .database
        .call(move |db| {
            db.query_row(
                "SELECT t.run_id,m.turn_id,t.generation FROM model_attempt m JOIN turn t ON t.id=m.turn_id WHERE m.id=?1",
                [&first_attempt_for_query],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();

    let first_run = first_scope.0.clone();
    request(
        &control,
        Command::RunStop(RunTarget {
            run_id: first_run.clone(),
        }),
    )
    .await;
    let stopped: String =
        control
            .store
            .database
            .call({
                let run = first_run.clone();
                move |db| {
                    Ok(db.query_row("SELECT state FROM run WHERE id=?1", [run], |row| row.get(0))?)
                }
            })
            .await
            .unwrap();
    assert!(
        matches!(stopped.as_str(), "completed" | "stopped"),
        "RunStop is terminal-idempotent: expected completed or stopped, got {stopped}"
    );
    let closed_preview = request(
        &control,
        Command::CompactPreview(Box::new(CompactPreviewArgs {
            thread_id: Some(target.clone()),
            ..Default::default()
        })),
    )
    .await;
    assert!(
        closed_preview["target_nodes"]["count"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "diagnostic summary-source target: {closed_preview:#?}"
    );
    assert_ne!(
        closed_preview["summary"],
        Value::Null,
        "snapshot preview must expose the candidate summary target"
    );
    assert_eq!(
        closed_preview["no_target"], false,
        "actual current path should have a normal closed target"
    );

    let mut second = proactive_third_turn_after_run(
        &control,
        &mut receive,
        &target,
        150,
        auth_veto_reply(),
        Some((&first_run, &first_scope.1)),
    )
    .await;
    let second_finished =
        finish_proactive_trace(&control, &mut receive, &mut second, &auth_veto_reply()).await;
    assert!(
        !second_finished.is_empty(),
        "new real Run has an exact served provider duty"
    );
    let all_events = proactive_events(&control, &target).await;
    // Count durable model-attempt records after RunStop, not only requests
    // observed after the helper's current-turn boundary; warmup can compact.
    let second_attempt = all_events
        .iter()
        .filter(|event| {
            event["metrics"]["summary_usage"]["requests"] == 1
                && event["compaction_outcome"]["outcome"]["reason"] == "provider_error"
        })
        .map(|event| event["attempt_id"].as_str().unwrap().to_owned())
        .find(|attempt| attempt != &first_attempt)
        .unwrap_or_else(|| {
            let facts: Vec<_> = all_events.iter().map(|event| json!({
                "attempt_id": event["attempt_id"], "state": event["state"],
                "reason": event["metrics"]["reason"], "outcome": event["compaction_outcome"]["outcome"],
                "summary_usage": event["metrics"]["summary_usage"], "error_details": event["error"]["details"]
            })).take(8).collect();
            panic!("new-Run provider attempt missing; admission_report={:#?}; helper_summary_scopes={:#?}; durable_event_facts={facts:#?}", second.admission_report, second.summary_scopes)
        });
    let fresh_trigger = all_events
        .iter()
        .find(|event| event["attempt_id"] == second_attempt)
        .expect("new-Run provider response trigger remains in durable events");
    assert_eq!(
        fresh_trigger["trigger"]["auto_at_tokens"], 100,
        "actual automatic trigger threshold: {fresh_trigger:#?}"
    );
    assert_eq!(
        fresh_trigger["trigger"]["input_tokens"],
        second.admission_report.as_ref().unwrap()["input_tokens"],
        "fresh successful report is the trigger's reported input: {fresh_trigger:#?}"
    );
    // The serviced trace already joined the provider attempt by report ID and token.
    let finished = second_finished
        .iter()
        .find(|finished| finished.boundary.report.attempt_id == second_attempt)
        .expect("the selected provider failure belongs to the serviced, finished trace");
    let second_record = automatic_attempt_record(&control, second_attempt.clone()).await;
    assert_eq!(second_record["token"], finished.boundary.gc_token);
    assert_eq!(second_record["outcome"], finished.outcome);
    assert_eq!(
        second_record["trigger"]["run_id"],
        second.admission_report.as_ref().unwrap()["run_id"],
        "persisted #A trigger belongs to the new Run"
    );
    assert!(
        second_record["trigger"]["a_input_tokens"]
            .as_u64()
            .is_some_and(|a| a > 80),
        "the exact terminal attempt retains the new Run's first-known #A: {second_record:#?}"
    );
    let second_scope = control
        .store
        .database
        .call(move |db| {
            db.query_row(
                "SELECT t.run_id,m.turn_id,t.generation FROM model_attempt m JOIN turn t ON t.id=m.turn_id WHERE m.id=?1",
                [&second_attempt],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_ne!(
        first_scope.0, second_scope.0,
        "different persisted real Run IDs"
    );
    assert_ne!(
        first_scope.1, second_scope.1,
        "the new Run owns its own Turn"
    );
    assert!(
        second
            .summary_scopes
            .iter()
            .all(|(run, turn)| run != &first_run && turn != &first_scope.1),
        "all helper-observed post-boundary requests belong to distinct live scope"
    );

    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
