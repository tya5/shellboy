//! Ephemeral per-Turn/generation budget; durable state remains in SQLite.
use crate::{model, protocol::ErrorCode};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reduction {
    OldGroups,
    CurrentSuffix,
}
pub(super) struct OverflowBudget {
    turn: String,
    generation: i64,
    stage: u8,
}
impl Default for OverflowBudget {
    fn default() -> Self {
        Self {
            turn: String::new(),
            generation: -1,
            stage: 0,
        }
    }
}
impl OverflowBudget {
    pub(super) fn observe(&mut self, turn: &str, generation: i64) {
        if self.turn != turn || self.generation != generation {
            self.turn = turn.into();
            self.generation = generation;
            self.stage = 0;
        }
    }
    pub(super) fn available(&self) -> bool {
        self.stage < 2
    }
    /// Called after the request failure has been durably saved.
    pub(super) fn begin(&mut self) -> Option<Reduction> {
        if !self.available() {
            return None;
        }
        Some(if self.stage == 0 {
            self.stage = 1;
            Reduction::OldGroups
        } else {
            self.stage = 2;
            Reduction::CurrentSuffix
        })
    }
    pub(super) fn succeeded(&mut self) {
        self.stage = 0;
    }
    #[cfg(test)]
    fn unlock_current(&mut self) {
        self.stage = 2;
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn retry_or_recover(
    control: &std::sync::Arc<super::Control>,
    snapshot: &super::Snapshot,
    _slot: &super::DriverSlot,
    attempt: String,
    failure: super::model::http::AttemptFailure,
    automatic_retries: u32,
    retry_limit: u32,
    retry_policy: super::retry::RetryPolicy,
    trim_not_applied: Option<serde_json::Value>,
    partial_attempts: Vec<String>,
) -> Result<(), super::ApiError> {
    use super::*;

    let exhausted = matches!(retry_policy, super::retry::RetryPolicy::Retry { .. })
        && automatic_retries >= retry_limit
        && retry_limit > 0;
    let reduction_unavailable = failure.error.is_context_limit()
        && trim_not_applied
            .as_ref()
            .is_some_and(|o| o["outcome"] != "adopted");
    let usage_limit = failure.error.usage_limit.as_deref().copied();
    let message = if let Some(fact) = usage_limit {
        fact.error().message
    } else if reduction_unavailable {
        format!(
            "{} (no further automatic input reduction available)",
            failure.error
        )
    } else if exhausted {
        format!(
            "{} (automatic retries exhausted: {automatic_retries}/{retry_limit})",
            failure.error
        )
    } else {
        failure.error.to_string()
    };
    let facts = control.compactor.recovery_facts(&snapshot.thread).await?;
    let mut error=ApiError::new(failure_code(&failure.error, failure.facts.status),message)
        .with_recovery_hint()
        .details(json!({"recovery":facts,"automatic_retries":automatic_retries,"retry_limit":retry_limit,
            "automatic_reduction_unavailable":reduction_unavailable,"last_failure_at":now(),"failure_kind":failure.error.kind,"http":failure.facts.http_observation}));
    if failure.error.is_context_limit() {
        error.hint = Some("Review the saved context with /compact before retrying.".into());
    }
    if let Some(cause) = failure.error.transport_cause {
        error.hint = Some(cause.next_action().into());
        if let Some(details) = &mut error.details {
            details["transport_cause"] = json!(cause);
        }
    }
    if let Some(fact) = usage_limit {
        error.hint = Some(if control.store.config.recovery.auto_retry_usage_limit
            && fact.kind == crate::model::usage_limit::LimitKind::UsageLimitReached
        {
            "An eligible wait retries once at the reported reset. To change the selection, use /model."
        } else {
            "Wait until the reported reset or select another connection/model with /model. No automatic switch or resume."
        }.into());
        if let Some(details) = &mut error.details {
            details["usage_limit"] = fact.details();
        }
    }
    if failure.error.kind == "incomplete" {
        let facts = model::content_filter::ContentFilterFacts::from_response(
            failure.raw_response.as_ref().unwrap_or(&Value::Null),
        );
        if facts.is_content_filter() {
            error.hint = Some(format!(
                "{} {}",
                error.hint.as_deref().unwrap_or(""),
                model::content_filter::NEXT_ACTIONS_HINT
            ));
        }
        if let Some(details) = &mut error.details {
            details["content_filter"] = json!(facts);
        }
    }
    let trusted_observation =
        trim_not_applied.map(super::trim_observation::DriverTrimObservation::from_reduction);
    recover_with_observation(
        control,
        snapshot,
        error.clone(),
        Some(attempt),
        trusted_observation,
        partial_attempts,
    )
    .await?;
    Err(error)
}
fn failure_code(error: &model::ModelError, http_status: Option<u16>) -> ErrorCode {
    let status = error
        .stream_provider_status
        .map(|fact| fact.value)
        .or(error.status)
        .or(http_status);
    if matches!(status, Some(408 | 429)) || error.usage_limit.is_some() || error.is_context_limit()
    {
        return ErrorCode::ModelRequestFailed;
    }
    match (error.kind.as_str(), status) {
        ("config_missing" | "config_missing_credentials", _) | (_, Some(401 | 403)) => {
            ErrorCode::ModelAuth
        }
        (_, Some(400..=499)) => ErrorCode::ModelRequestRejected,
        _ => ErrorCode::ModelRequestFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_failure_families_use_status_and_local_kind_not_prose() {
        for (kind, status, expected) in [
            ("config_missing", None, ErrorCode::ModelAuth),
            ("provider", Some(401), ErrorCode::ModelAuth),
            ("provider", Some(403), ErrorCode::ModelAuth),
            ("provider", Some(404), ErrorCode::ModelRequestRejected),
            ("provider", Some(422), ErrorCode::ModelRequestRejected),
            ("provider", Some(408), ErrorCode::ModelRequestFailed),
            ("provider", Some(429), ErrorCode::ModelRequestFailed),
            ("provider", Some(503), ErrorCode::ModelRequestFailed),
            ("protocol", None, ErrorCode::ModelRequestFailed),
        ] {
            let mut error = model::ModelError::new(kind, "Invalid key; please authenticate");
            error.status = status;
            assert_eq!(failure_code(&error, None), expected);
        }
        let mut context = model::ModelError::new("provider", "request did not fit");
        context.status = Some(400);
        context.provider_code = Some("context_length_exceeded".into());
        assert_eq!(failure_code(&context, None), ErrorCode::ModelRequestFailed);
    }
    #[test]
    fn two_stage_budget_is_shared_by_requests_and_reset_only_by_turn_or_generation() {
        let mut budget = OverflowBudget::default();
        budget.observe("turn", 1);
        assert!(budget.available());
        assert_eq!(budget.begin(), Some(Reduction::OldGroups));
        budget.observe("turn", 1);
        assert_eq!(budget.begin(), Some(Reduction::CurrentSuffix));
        assert!(!budget.available());
        assert_eq!(budget.begin(), None);
        budget.observe("turn", 1);
        assert!(!budget.available());
        budget.observe("turn", 2);
        assert_eq!(budget.begin(), Some(Reduction::OldGroups));
        budget.unlock_current();
        assert!(
            !budget.available(),
            "no old target consumes the current-suffix stage too"
        );
        budget.observe("next", 2);
        assert!(budget.available());
        assert_eq!(budget.begin(), Some(Reduction::OldGroups));
    }
}
