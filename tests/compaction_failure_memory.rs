//! Public S2b provider-memory controls using the shared real-owner loopback fixture.
use super::*;

/// Repeated eligible reports in one real Run/Turn/generation/owner scope may
/// reuse one provider invocation, even when each request is a terminal veto.
#[tokio::test]
async fn provider_veto_is_one_invocation_per_real_scope() {
    let (summaries, records) = ax_growing_turn_summary_requests(|| {
        (
            401,
            json!({"error":{"code":"invalid_request_error","message":"synthetic auth veto"}}),
        )
    }, 0)
    .await;

    // The fixture generates 17 eligible reports while preserving one real
    // owner, Run, Turn, generation, and original closed-range identity. A
    // report outcome is not the same thing as an admitted provider invocation.
    assert_eq!(records.len(), 17, "one report outcome per eligible report: {records:#?}");
    assert_eq!(summaries, 1, "provider scope admits one invocation: {records:#?}");

    let admitted: Vec<_> = records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "provider_error")
        .collect();
    assert_eq!(admitted.len(), 1, "one actual provider failure: {records:#?}");
    let outcome = &admitted[0]["compaction_outcome"]["outcome"];
    assert_eq!(outcome["kind"], "failed");
    assert_eq!(outcome["origin"], "provider_response");
    assert_eq!(outcome["http_status"], 401);
    for field in ["provider_status", "provider_status_source", "final_group_count", "final_source_nodes"] {
        assert!(outcome.get(field).is_none(), "{field}: {outcome:#?}");
    }
    assert_eq!(admitted[0]["error"]["details"]["model_requests"], 1);
    assert_eq!(admitted[0]["error"]["details"]["summary_usage"]["requests"], 1);

    let suppressed: Vec<_> = records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "retry_suppressed")
        .collect();
    assert_eq!(suppressed.len(), 16, "remaining reports are honestly suppressed: {records:#?}");
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


/// Every actual HTTP veto class outranks the numeric/refusal-looking body and
/// shares the one-invocation provider policy for the captured real scope.
#[tokio::test]
async fn auth_not_found_and_rate_vetoes_share_provider_scope_without_halving() {
    for (status, reply) in [
        (403, forbidden_reply as fn() -> (u16, Value)),
        (404, not_found_reply as fn() -> (u16, Value)),
        (429, rate_limit_reply as fn() -> (u16, Value)),
    ] {
        let (summaries, records) = ax_growing_turn_summary_requests(reply, 0).await;
        assert_eq!(records.len(), 17, "HTTP {status}: one report outcome per eligible report: {records:#?}");
        assert_eq!(summaries, 1, "HTTP {status}: one invocation in the real scope: {records:#?}");
        let provider_failures: Vec<_> = records
            .iter()
            .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "provider_error")
            .collect();
        assert_eq!(provider_failures.len(), 1, "HTTP {status}: {records:#?}");
        let outcome = &provider_failures[0]["compaction_outcome"]["outcome"];
        assert_eq!(outcome["origin"], "provider_response", "HTTP {status}: {outcome:#?}");
        assert_eq!(outcome["http_status"], status, "HTTP {status}: {outcome:#?}");
        for field in ["final_group_count", "final_source_nodes"] {
            assert!(outcome.get(field).is_none(), "HTTP {status}, {field}: {outcome:#?}");
        }
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
    let (summaries, records) = ax_growing_turn_summary_requests(retryable_server_error_reply, 2).await;
    assert_eq!(summaries, 3, "one invocation has its configured retry vector: {records:#?}");
    assert_eq!(records.len(), 17, "one report outcome per eligible report: {records:#?}");
    let admitted: Vec<_> = records
        .iter()
        .filter(|record| record["compaction_outcome"]["outcome"]["reason"] == "provider_error")
        .collect();
    assert_eq!(admitted.len(), 1, "one provider-scoped invocation: {records:#?}");
    assert_eq!(admitted[0]["error"]["details"]["model_requests"], 3);
    assert_eq!(admitted[0]["error"]["details"]["summary_usage"]["requests"], 3);
}

fn retryable_server_error_reply() -> (u16, Value) {
    (500, json!({"error":{"code":"server_error","message":"synthetic transient failure"}}))
}
