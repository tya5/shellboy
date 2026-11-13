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
    })
    .await;

    // The fixture generates 17 eligible reports while preserving one real
    // owner, Run, Turn, generation, and original closed-range identity.
    assert_eq!(records.len(), 17, "one outcome per eligible report: {records:#?}");
    assert_eq!(summaries, 1, "provider scope admits one invocation: {records:#?}");
}
