//! Public S2b provider-memory controls using the shared real-owner loopback fixture.
use super::*;

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
    assert_eq!(admitted[0]["metrics"]["model_requests"], admitted_requests);
    assert_eq!(
        admitted[0]["metrics"]["summary_usage"]["requests"],
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
            record["metrics"]["model_requests"].is_null()
                && record["metrics"]["summary_usage"].is_null(),
            "suppression must omit request and usage facts: {record:#?}"
        );
    }
}
