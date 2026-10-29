//! Consecutive conversation retries; the HTTP client reports facts only.
use super::*;
use model::http::{ResponseFacts, ResponseReceived};
use std::time::Duration;

const MAX_RETRY_AFTER: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryPolicy {
    Ineligible,
    Retry { partial_output: bool },
    Deferred(Duration),
}

/// The single policy boundary for a new chargeable conversation request.
/// Transport readers report observations, not permission to resend.
fn classify_retry(
    error: &model::ModelError,
    facts: &ResponseFacts,
    retry_partial: bool,
) -> RetryPolicy {
    if facts.terminal_received || error.is_context_limit() {
        return RetryPolicy::Ineligible;
    }
    let transient_status = matches!(facts.status, Some(429 | 500 | 502 | 503 | 504));
    let eligible = match error.kind.as_str() {
        "http" | "provider" => transient_status,
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

/// Large-input activation is distinct from the ordinary transient retry list:
/// every 5xx may benefit, but terminal output, partial opt-out and deferred
/// Retry-After still prohibit an automatic new request.
fn eligible_trim_failure(
    error: &model::ModelError,
    facts: &ResponseFacts,
    retry_partial: bool,
) -> bool {
    if facts.terminal_received
        || error.is_context_limit()
        || (facts.received == ResponseReceived::Output && !retry_partial)
        || facts
            .retry_after
            .is_some_and(|delay| delay > MAX_RETRY_AFTER)
        || facts
            .http_observation
            .is_some_and(|http| http.phase == model::http::HttpPhase::BeforeBodySource)
    {
        return false;
    }
    let server_failure = facts
        .status
        .is_some_and(|status| (500..600).contains(&status));
    match error.kind.as_str() {
        "http" | "provider" => server_failure,
        "transport" | "incomplete" => {
            server_failure
                || (facts.http_observation.is_some()
                    && facts
                        .status
                        .is_none_or(|status| (200..300).contains(&status)))
        }
        "timeout" => {
            server_failure
                || (facts
                    .http_observation
                    .is_some_and(|http| http.phase != model::http::HttpPhase::BodyReceive)
                    && facts
                        .status
                        .is_none_or(|status| (200..300).contains(&status)))
        }
        _ => false,
    }
}

#[derive(Default)]
pub(super) struct RetryBudget {
    turn: String,
    generation: i64,
    used: u32,
    trim_used: bool,
    trim_resume_pending: bool,
    upload_failures: u8,
}
impl RetryBudget {
    pub(super) fn observe(&mut self, turn: &str, generation: i64) {
        if self.turn != turn || self.generation != generation {
            self.turn = turn.into();
            self.generation = generation;
            self.used = 0;
            self.trim_used = false;
            self.trim_resume_pending = false;
            self.upload_failures = 0;
        }
    }
    /// Reset only after the completed response has been durably adopted.
    pub(super) fn succeeded(&mut self) {
        self.used = 0;
        self.upload_failures = 0;
        self.trim_resume_pending = false;
    }
    /// One reduction/resumption allowance per Turn/generation; successful tool
    /// responses reset retries but never replenish this allowance.
    pub(super) fn trim_available(&self, enabled: bool) -> bool {
        enabled && !self.trim_used
    }
    pub(super) fn consume_trim(&mut self) {
        self.trim_used = true;
        self.trim_resume_pending = true;
    }
    fn repeated_upload_failure(&mut self, incomplete: bool) -> bool {
        self.upload_failures = if incomplete {
            self.upload_failures.saturating_add(1)
        } else {
            0
        };
        self.upload_failures >= 2
    }
    fn delay(&self, limit: u32, cap: Duration, retry_after: Option<Duration>) -> Option<Duration> {
        (self.used < limit).then(|| {
            retry_after
                .unwrap_or_else(|| Duration::from_millis(1_000u64 << self.used.min(20)).min(cap))
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn retry_or_recover(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    attempt: String,
    mut failure: model::http::AttemptFailure,
    input_characters: u64,
    budget: &mut RetryBudget,
    overflow: &mut OverflowBudget,
    boundary_guard: tokio::sync::MutexGuard<'_, ()>,
) -> Result<(), ApiError> {
    let connection = &control.store.config.connections[&snapshot.profile];
    let limit = connection.response_max_retries;
    let mut policy = classify_retry(
        &failure.error,
        &failure.facts,
        connection.response_retry_partial,
    );
    let mut trim_not_applied = None;
    let upload_incomplete = failure
        .facts
        .http_observation
        .is_some_and(|http| http.is_known_upload_incomplete());
    let repeated_upload_failure = budget.repeated_upload_failure(upload_incomplete);
    if repeated_upload_failure {
        // A second incomplete body source is not evidence that reducing context
        // will fix the transport. Keep its observation and stop spending.
        policy = RetryPolicy::Ineligible;
        failure.error.message.push_str("; repeated incomplete request-body source; inspect the connection or local network before explicitly retrying");
    }
    let eligible_trim_failure = eligible_trim_failure(
        &failure.error,
        &failure.facts,
        connection.response_retry_partial,
    );
    let mut no_candidates = false;
    // A previously rejected edit leaves the one-time trim right available,
    // but cannot override the second partial-upload failure's spending cap.
    if !repeated_upload_failure
        && budget.trim_available(connection.auto_trim_on_disconnect)
        && eligible_trim_failure
    {
        let trigger_tokens = control
            .compactor
            .input_tokens(
                &snapshot.thread,
                &snapshot.profile,
                &snapshot.model,
                input_characters,
            )
            .await?;
        if trigger_tokens > connection.compact_hint_tokens.unwrap_or(75_000) {
            match trim::try_trim(
                control,
                snapshot,
                slot,
                &attempt,
                &mut failure,
                trigger_tokens,
                budget.used,
                limit,
                &mut trim_not_applied,
            )
            .await?
            {
                trim::Outcome::Adopted => {
                    budget.consume_trim();
                    control.changed(&snapshot.thread);
                    drop(boundary_guard);
                    if let Some(delay) = failure.facts.retry_after {
                        tokio::select! { biased; _ = slot.stop.cancelled() => {}, _ = tokio::time::sleep(delay) => {} }
                    }
                    return Ok(());
                }
                trim::Outcome::NoCandidates => no_candidates = true,
                trim::Outcome::Rejected => {}
            }
            if slot.stop.is_cancelled() {
                return Ok(());
            }
        }
    }
    if budget.trim_resume_pending || no_candidates {
        // Only a failure of the first resumed POST is guarded. A successfully
        // adopted response clears this guard but never renews the trim right.
        policy = RetryPolicy::Ineligible;
        overflow.unlock_current();
    }
    let delay = if matches!(policy, RetryPolicy::Retry { .. }) {
        budget.delay(
            limit,
            Duration::from_millis(connection.response_retry_max_delay_ms),
            failure.facts.retry_after,
        )
    } else {
        None
    };
    if let Some(delay) = delay {
        let partial = matches!(
            policy,
            RetryPolicy::Retry {
                partial_output: true
            }
        );
        let body = json!({"error":failure.error,"partial_text":failure.partial_text,
            "partial_reasoning":failure.partial_reasoning,"completed_items":failure.completed_items,
            "raw_response":failure.raw_response,"attempt_id":attempt,
            "automatic_retry":{"number":budget.used+1,"limit":limit,
                "delay_ms":delay.as_millis() as u64,"partial_response_discarded":partial,
                "possible_duplicate_charge":true}});
        let saved_attempt = attempt.clone();
        let turn = snapshot.turn.clone();
        let transition = control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let transition =
                    super::super::state::fail_attempt(&tx, &saved_attempt, &body, &now())?;
                if transition == super::super::state::AttemptTransition::Applied && partial {
                    save_received_failure(&tx, &turn, &body)?;
                }
                tx.commit()?;
                Ok(transition)
            })
            .await
            .map_err(storage_error)?;
        if transition != super::super::state::AttemptTransition::Applied {
            return Ok(());
        }
        budget.used += 1;
        control.changed(&snapshot.thread);
        // Admission is released while waiting; ordinary boundaries are checked
        // again before the next POST. Stop/shutdown wins over even a zero delay.
        drop(boundary_guard);
        tokio::select! {
            biased;
            _ = slot.stop.cancelled() => {},
            _ = tokio::time::sleep(delay) => {},
        }
        return Ok(());
    }
    if let RetryPolicy::Deferred(delay) = policy {
        failure.error.message.push_str(&format!(
            "; server requested Retry-After {}s; automatic wait limit is 600s",
            delay.as_secs()
        ));
    }
    overflow::retry_or_recover(
        control,
        snapshot,
        slot,
        attempt,
        failure,
        overflow,
        budget.used,
        limit,
        policy,
        trim_not_applied,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_budget_resets_only_after_adoption_or_turn_generation_change() {
        let mut budget = RetryBudget::default();
        budget.observe("t", 1);
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            assert_eq!(
                budget.delay(8, Duration::from_secs(60), None),
                Some(Duration::from_secs(seconds))
            );
            budget.used += 1;
            budget.observe("t", 1);
        }
        assert_eq!(budget.delay(8, Duration::from_secs(60), None), None);
        budget.succeeded();
        assert_eq!(
            budget.delay(8, Duration::from_secs(60), None),
            Some(Duration::from_secs(1))
        );
        budget.used = 8;
        budget.observe("t", 2);
        assert_eq!(budget.used, 0);
        assert_eq!(budget.delay(0, Duration::from_secs(60), None), None);
        budget.used = 8;
        budget.observe("other", 2);
        assert_eq!(budget.used, 0);
    }
    #[test]
    fn server_wait_is_not_shortened_by_exponential_cap() {
        let budget = RetryBudget::default();
        for seconds in [0, 1, 120, 600] {
            assert_eq!(
                budget.delay(
                    8,
                    Duration::from_millis(10),
                    Some(Duration::from_secs(seconds))
                ),
                Some(Duration::from_secs(seconds))
            );
        }
    }
    #[test]
    fn one_policy_classifies_error_status_reception_terminal_and_connection_optout() {
        use ResponseReceived::{Metadata, None as NoResponse, Output};
        for kind in [
            "http",
            "provider",
            "transport",
            "timeout",
            "incomplete",
            "protocol",
            "cancelled",
            "consumer_closed",
        ] {
            for status in [
                None,
                Some(200),
                Some(400),
                Some(401),
                Some(403),
                Some(404),
                Some(429),
                Some(500),
                Some(501),
                Some(502),
                Some(503),
                Some(504),
            ] {
                for received in [NoResponse, Metadata, Output] {
                    for terminal_received in [false, true] {
                        for partial_enabled in [false, true] {
                            let facts = ResponseFacts {
                                status,
                                received,
                                terminal_received,
                                retry_after: None,
                                http_observation: None,
                            };
                            let error = model::ModelError::new(kind, "fixture");
                            let expected = !terminal_received
                                && (received != Output || partial_enabled)
                                && match kind {
                                    "http" | "provider" => {
                                        matches!(status, Some(429 | 500 | 502 | 503 | 504))
                                    }
                                    "transport" | "timeout" | "incomplete" => {
                                        status.is_none()
                                            || status == Some(200)
                                            || matches!(status, Some(429 | 500 | 502 | 503 | 504))
                                    }
                                    _ => false,
                                };
                            assert_eq!(
                                classify_retry(&error, &facts, partial_enabled),
                                if expected {
                                    RetryPolicy::Retry {
                                        partial_output: received == Output,
                                    }
                                } else {
                                    RetryPolicy::Ineligible
                                },
                                "{kind} {facts:?} partial={partial_enabled}"
                            );
                        }
                    }
                }
            }
        }
        let mut error = model::ModelError::new("provider", "fixture");
        error.provider_code = Some("context_length_exceeded".into());
        let mut facts = ResponseFacts {
            status: Some(503),
            ..Default::default()
        };
        assert_eq!(
            classify_retry(&error, &facts, true),
            RetryPolicy::Ineligible
        );
        error.provider_code = None;
        for seconds in [0, 600, 601] {
            facts.retry_after = Some(Duration::from_secs(seconds));
            assert_eq!(
                classify_retry(&error, &facts, true),
                if seconds > 600 {
                    RetryPolicy::Deferred(Duration::from_secs(seconds))
                } else {
                    RetryPolicy::Retry {
                        partial_output: false,
                    }
                }
            );
        }
    }
}

#[cfg(test)]
mod trim_budget_tests {
    use super::*;
    #[test]
    fn trim_is_available_before_retries_and_not_replenished_by_success() {
        let mut budget = RetryBudget::default();
        budget.observe("turn", 1);
        assert!(!budget.trim_available(false));
        assert!(budget.trim_available(true));
        budget.consume_trim();
        assert!(budget.trim_resume_pending);
        budget.succeeded();
        assert!(
            !budget.trim_resume_pending,
            "adoption clears only the immediate resume guard"
        );
        assert!(!budget.trim_available(true));
        budget.observe("turn", 1);
        assert!(!budget.trim_available(true));
        budget.observe("turn", 2);
        assert!(budget.trim_available(true));
        budget.consume_trim();
        budget.observe("other", 2);
        assert!(budget.trim_available(true));
    }
    #[test]
    fn repeated_incomplete_body_sources_stop_after_two_and_reset_on_other_failure_or_success() {
        let mut budget = RetryBudget::default();
        budget.observe("turn", 0);
        assert!(!budget.repeated_upload_failure(true));
        assert!(!budget.repeated_upload_failure(false));
        assert!(!budget.repeated_upload_failure(true));
        assert!(budget.repeated_upload_failure(true));
        budget.succeeded();
        assert!(!budget.repeated_upload_failure(true));
        budget.observe("turn", 1);
        assert!(!budget.repeated_upload_failure(true));
    }
    #[test]
    fn early_trim_separates_all_server_failures_and_header_timeouts_from_idle_or_upload_failures() {
        use model::http::{ConnectionReuse, HttpObservation, HttpPhase};
        let mut facts = ResponseFacts {
            http_observation: Some(HttpObservation {
                phase: HttpPhase::BodyReceive,
                request_body_bytes: 100,
                body_source_bytes_consumed: 100,
                header_elapsed_ms: Some(10),
                pool_generation: 0,
                connection_reuse: ConnectionReuse::FreshPool,
            }),
            ..Default::default()
        };
        for status in [500, 501, 502, 503, 504, 599] {
            facts.status = Some(status);
            assert!(eligible_trim_failure(
                &model::ModelError::new("provider", "fixture"),
                &facts,
                true
            ));
            assert!(eligible_trim_failure(
                &model::ModelError::new("timeout", "error body stalled"),
                &facts,
                true
            ));
        }
        facts.status = Some(429);
        assert!(!eligible_trim_failure(
            &model::ModelError::new("timeout", "rate limit body stalled"),
            &facts,
            true
        ));
        facts.status = Some(200);
        assert!(!eligible_trim_failure(
            &model::ModelError::new("timeout", "body idle"),
            &facts,
            true
        ));
        facts.status = None;
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::AwaitingHeadersDeliveryUnknown;
        assert!(eligible_trim_failure(
            &model::ModelError::new("timeout", "header timeout"),
            &facts,
            true
        ));
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::UploadSourceIncomplete;
        facts
            .http_observation
            .as_mut()
            .unwrap()
            .body_source_bytes_consumed = 50;
        assert!(eligible_trim_failure(
            &model::ModelError::new("timeout", "upload incomplete"),
            &facts,
            true
        ));
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::BeforeBodySource;
        facts
            .http_observation
            .as_mut()
            .unwrap()
            .body_source_bytes_consumed = 0;
        assert!(!eligible_trim_failure(
            &model::ModelError::new("transport", "connect failed"),
            &facts,
            true
        ));
        let setup = ResponseFacts::default();
        let setup_error =
            model::ModelError::new("transport", "client reconstruction failed before sending");
        assert!(!eligible_trim_failure(&setup_error, &setup, true));
        assert!(
            matches!(
                classify_retry(&setup_error, &setup, true),
                RetryPolicy::Retry { .. }
            ),
            "failure before transport observation retains ordinary retry without spending trim"
        );
        facts.http_observation.as_mut().unwrap().phase = HttpPhase::BodyReceive;
        facts.status = Some(503);
        facts.retry_after = Some(Duration::from_secs(601));
        assert!(!eligible_trim_failure(
            &model::ModelError::new("provider", "fixture"),
            &facts,
            true
        ));
        facts.retry_after = None;
        facts.received = ResponseReceived::Output;
        assert!(!eligible_trim_failure(
            &model::ModelError::new("provider", "fixture"),
            &facts,
            false
        ));
        facts.received = ResponseReceived::None;
        facts.terminal_received = true;
        assert!(!eligible_trim_failure(
            &model::ModelError::new("provider", "fixture"),
            &facts,
            true
        ));
    }
}
