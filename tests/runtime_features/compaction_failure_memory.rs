//! Public S2b provider-memory controls using the shared real-owner loopback fixture.
use super::*;

/// Repeated eligible reports in one real Run/Turn/generation/owner scope may
/// reuse one provider invocation, even when each request is a terminal veto.
#[tokio::test]
async fn provider_veto_is_one_invocation_per_real_scope() {
    let result = ax_growing_turn_summary_requests(auth_veto_reply, 0, None).await;
    assert_scope_trace(&result);
    assert_provider_scope_records(&result, 401, 1);
}

/// The fixture exposes actual owner instance, Run, Turn and generation through
/// the persisted model-attempt/Turn join. E's endpoint IDs are intentionally
/// not in public trace payloads; unchanged original history is checked here via
/// the captured path prefix, so the exact private E digest remains a fixture-only
/// identity proof gap rather than a fabricated/public DTO assertion.
fn assert_scope_trace(result: &AxFixtureResult) {
    assert!(!result.owner_instance_id.is_empty());
    assert_eq!(result.report_scopes.len(), 17);
    let first = result.report_scopes.first().unwrap();
    assert!(
        result.report_scopes.iter().all(|scope| scope == first),
        "actual Run/Turn/generation changed: {:?}",
        result.report_scopes
    );
    assert!(
        result.final_path_ids.starts_with(&result.initial_path_ids),
        "the original path prefix changed during report growth"
    );
}

/// Assert all admitted and suppressed reports, not only the aggregate HTTP
/// count. Suppression is an honest preparation outcome with no request facts.
fn assert_provider_scope_records(result: &AxFixtureResult, status: u16, admitted_requests: u64) {
    assert_eq!(
        result.summaries, admitted_requests as usize,
        "actual synthetic HTTP summary POST count: {:#?}",
        result.records
    );
    assert_eq!(
        result.records.len(),
        17,
        "one report outcome per eligible report: {:#?}",
        result.records
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
    let record = admitted[0];
    let outcome = &record["compaction_outcome"]["outcome"];
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
        record["error"]["details"]["model_requests"],
        admitted_requests
    );
    assert_eq!(
        record["error"]["details"]["summary_usage"]["requests"],
        admitted_requests
    );

    let suppressed: Vec<_> = result
        .records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "retry_suppressed")
        .collect();
    assert_eq!(
        suppressed.len(),
        16,
        "all remaining reports must be suppressed: {:#?}",
        result.records
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
        let result = ax_growing_turn_summary_requests(reply, 0, None).await;
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
    let result = ax_growing_turn_summary_requests(retryable_server_error_reply, 2, None).await;
    assert_eq!(
        result.summaries, 3,
        "one invocation has its configured retry vector: {:#?}",
        result.records
    );
    assert_scope_trace(&result);
    assert_provider_scope_records(&result, 500, 3);
}

fn retryable_server_error_reply() -> (u16, Value) {
    (
        500,
        json!({"error":{"code":"server_error","message":"synthetic transient failure"}}),
    )
}

/// UnknownInvalid never suppresses, repairs, or overwrites the seeded slot.
/// Values are synthetic; only storage type and bytes are checked in the fixture.
#[tokio::test]
async fn unknown_slot_values_allow_provider_calls_and_remain_untouched() {
    let cases = [
        (FailureSlotSeed::Malformed64, "text", vec![b'g'; 64]),
        (FailureSlotSeed::Uppercase64, "text", vec![b'A'; 64]),
        (FailureSlotSeed::OversizeText, "text", vec![b'a'; 65_536]),
        (FailureSlotSeed::Blob, "blob", vec![b'a'; 64]),
        (
            FailureSlotSeed::InvalidUtf8,
            "text",
            [vec![0xff], vec![0; 63]].concat(),
        ),
    ];
    for (seed, expected_type, expected_bytes) in cases {
        let result = ax_growing_turn_summary_requests(auth_veto_reply, 0, Some(seed)).await;
        assert_eq!(
            result.summaries, 17,
            "unknown slot permits every eligible provider call"
        );
        assert_eq!(result.records.len(), 17);
        assert_eq!(result.failure_memory_slots, 1, "no second slot was created");
        assert_eq!(
            result.seeded_slot,
            Some((expected_type.to_owned(), expected_bytes)),
            "the original invalid slot must remain byte-for-byte and type-for-type unchanged"
        );
        assert!(
            result.records.iter().all(|record| {
                record["compaction_outcome"]["outcome"]["reason"] == "provider_error"
                    && record["compaction_outcome"]["outcome"]["origin"] == "provider_response"
                    && record["compaction_outcome"]["outcome"]["http_status"] == 401
                    && record["error"]["details"]["model_requests"] == 1
                    && record["error"]["details"]["summary_usage"]["requests"] == 1
            }),
            "unknown slot must not create suppressions or falsify request accounting"
        );
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

    let first_trace = proactive_third_turn_after_run(
        &control,
        &mut receive,
        &target,
        150,
        auth_veto_reply(),
        None,
    )
    .await;
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

    let second = proactive_third_turn_after_run(
        &control,
        &mut receive,
        &target,
        150,
        auth_veto_reply(),
        Some((&first_run, &first_scope.1)),
    )
    .await;
    let all_events = proactive_events(&control, &target).await;
    assert!(
        second.summary_requests > 0,
        "a distinct live Run/Turn admits provider calls across the entire warmup boundary: {all_events:#?}"
    );
    assert_eq!(second.summary_requests, second.summary_scopes.len());
    assert!(
        second
            .summary_scopes
            .iter()
            .all(|(run, turn)| run != &first_run && turn != &first_scope.1)
    );
    assert_eq!(
        second.summary_bodies.first(),
        first_trace.summary_bodies.first(),
        "the re-opened request has the same serialized summary input (same E source and settings)"
    );
    let second_attempt = all_events
        .iter()
        .filter(|event| event["metrics"]["summary_usage"]["requests"] == 1)
        .map(|event| event["attempt_id"].as_str().unwrap().to_owned())
        .find(|attempt| attempt != &first_attempt)
        .expect("new-Run provider attempt exists");
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

    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}
