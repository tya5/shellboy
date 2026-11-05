//! The single transient-retry classification shared by conversation and summary
//! requests. Transport readers report observations, not permission to resend.
use super::ModelError;
use super::http::{ResponseFacts, ResponseReceived};
use std::time::Duration;

pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryPolicy {
    Ineligible,
    Retry { partial_output: bool },
    Deferred(Duration),
}

/// The single policy boundary for a new chargeable conversation request.
/// Transport readers report observations, not permission to resend.
pub(crate) fn classify_retry(
    error: &ModelError,
    facts: &ResponseFacts,
    retry_partial: bool,
) -> RetryPolicy {
    if facts.terminal_received && !error.is_context_limit() {
        return RetryPolicy::Ineligible;
    }
    if error.is_context_limit() {
        if let Some(delay) = facts.retry_after
            && delay > MAX_RETRY_AFTER
        {
            return RetryPolicy::Deferred(delay);
        }
        return if facts.received == ResponseReceived::Output && !retry_partial {
            RetryPolicy::Ineligible
        } else {
            RetryPolicy::Retry {
                partial_output: facts.received == ResponseReceived::Output,
            }
        };
    }
    let transient_status = matches!(facts.status, Some(429 | 500..=599));
    let eligible = match error.kind.as_str() {
        "http" | "provider" => transient_status,
        "usage_limit" => transient_status && error.usage_limit.is_some(),
        // An unfinished error body may fail before it can be decoded. Its
        // saved retryable HTTP status remains authoritative for this policy.
        "transport" | "timeout" | "incomplete" => {
            transient_status
                || facts
                    .status
                    .is_none_or(|status| (200..300).contains(&status))
        }
        _ => false,
    };
    let partial_output = facts.received == ResponseReceived::Output;
    if !eligible || (partial_output && !retry_partial) {
        return RetryPolicy::Ineligible;
    }
    if let Some(delay) = facts.retry_after
        && delay > MAX_RETRY_AFTER
    {
        return RetryPolicy::Deferred(delay);
    }
    RetryPolicy::Retry { partial_output }
}
