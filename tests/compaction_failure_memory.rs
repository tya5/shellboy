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
fn assert_scope_trace(result: &AxFixtureResult) {
    assert!(!result.owner_instance_id.is_empty());
    assert_eq!(result.report_scopes.len(), 17);
    let first = result.report_scopes.first().unwrap();
    assert!(result.report_scopes.iter().all(|scope| scope == first), "actual Run/Turn/generation changed: {:?}", result.report_scopes);
    assert!(result.final_path_ids.starts_with(&result.initial_path_ids), "the original path prefix changed during report growth");
}

/// Assert all admitted and suppressed reports, not only the aggregate HTTP
/// count. Suppression is an honest preparation outcome with no request facts.
fn assert_provider_scope_records(result: &AxFixtureResult, status: u16, admitted_requests: u64) {
    assert_eq!(result.records.len(), 17, "one report outcome per eligible report: {:#?}", result.records);
    let admitted: Vec<_> = result.records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "provider_error")
        .collect();
    assert_eq!(admitted.len(), 1, "one provider response outcome: {:#?}", result.records);
    let record = admitted[0];
    let outcome = &record["compaction_outcome"]["outcome"];
    assert_eq!(outcome["kind"], "failed");
    assert_eq!(outcome["origin"], "provider_response");
    assert_eq!(outcome["http_status"], status);
    for field in ["provider_status", "provider_status_source", "final_group_count", "final_source_nodes"] {
        assert!(outcome.get(field).is_none(), "{field}: {outcome:#?}");
    }
    assert_eq!(record["error"]["details"]["model_requests"], admitted_requests);
    assert_eq!(record["error"]["details"]["summary_usage"]["requests"], admitted_requests);

    let suppressed: Vec<_> = result.records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "retry_suppressed")
        .collect();
    assert_eq!(suppressed.len(), 16, "all remaining reports must be suppressed: {:#?}", result.records);
    for record in suppressed {
        let outcome = &record["compaction_outcome"]["outcome"];
        assert_eq!(outcome["kind"], "failed");
        assert_eq!(outcome["origin"], "preparation");
        for field in ["http_status", "provider_status", "provider_status_source", "final_group_count", "final_source_nodes"] {
            assert!(outcome.get(field).is_none(), "{field}: {outcome:#?}");
        }
        assert_eq!(record["error"]["details"]["model_requests"], 0);
        assert_eq!(record["error"]["details"]["summary_usage"]["requests"], 0);
    }
}

fn auth_veto_reply() -> (u16, Value) {
    (401, json!({"error":{"code":"invalid_request_error","message":"synthetic auth veto"}}))
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
        assert_eq!(result.summaries, 1, "HTTP {status}: one invocation in the real scope");
        assert_scope_trace(&result);
        assert_provider_scope_records(&result, status, 1);
    }
}

fn forbidden_reply() -> (u16, Value) {
    (403, json!({"error":{"code":"invalid_request_error","message":"synthetic veto"}}))
}
fn not_found_reply() -> (u16, Value) {
    (404, json!({"error":{"code":"invalid_request_error","message":"synthetic veto"}}))
}
fn rate_limit_reply() -> (u16, Value) {
    (429, json!({"error":{"code":"invalid_request_error","message":"synthetic veto"}}))
}


/// The memory unit is one admitted invocation, not one individual HTTP POST.
/// With two configured transient retries, the first report makes three POSTs;
/// the repeated same-scope report is suppressed without another invocation.
#[tokio::test]
async fn provider_scope_counts_invocation_not_transient_posts() {
    let result = ax_growing_turn_summary_requests(retryable_server_error_reply, 2).await;
    assert_eq!(result.summaries, 3, "one invocation has its configured retry vector: {:#?}", result.records);
    assert_scope_trace(&result);
    assert_provider_scope_records(&result, 500, 3);
}

fn retryable_server_error_reply() -> (u16, Value) {
    (500, json!({"error":{"code":"server_error","message":"synthetic transient failure"}}))
}
