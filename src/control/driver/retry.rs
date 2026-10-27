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

#[derive(Default)]
pub(super) struct RetryBudget {
    turn: String,
    generation: i64,
    used: u32,
    trim_used: bool,
}
impl RetryBudget {
    pub(super) fn observe(&mut self, turn: &str, generation: i64) {
        if self.turn != turn || self.generation != generation {
            self.turn = turn.into();
            self.generation = generation;
            self.used = 0;
            self.trim_used = false;
        }
    }
    /// Reset only after the completed response has been durably adopted.
    pub(super) fn succeeded(&mut self) {
        self.used = 0;
    }
    /// This safety net is consumed once per Turn/generation, even if no safe
    /// reduction exists. Ordinary successful tool responses do not replenish it.
    fn begin_trim(&mut self, enabled: bool, limit: u32, policy: RetryPolicy) -> bool {
        if !enabled
            || limit == 0
            || self.used < limit
            || self.trim_used
            || !matches!(policy, RetryPolicy::Retry { .. })
        {
            return false;
        }
        self.trim_used = true;
        true
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
    budget: &mut RetryBudget,
    overflow: &mut OverflowBudget,
    boundary_guard: tokio::sync::MutexGuard<'_, ()>,
) -> Result<(), ApiError> {
    let connection = &control.store.config.connections[&snapshot.profile];
    let limit = connection.response_max_retries;
    let policy = classify_retry(
        &failure.error,
        &failure.facts,
        connection.response_retry_partial,
    );
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
    if budget.begin_trim(connection.auto_trim_on_disconnect, limit, policy)
        && trim::try_trim(
            control,
            snapshot,
            slot,
            &attempt,
            &mut failure,
            connection.compact_hint_tokens,
            budget.used,
            limit,
        )
        .await?
    {
        // Only an adopted reduction renews consecutive retry capacity.
        budget.succeeded();
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
    fn disconnect_trim_is_once_per_turn_generation_and_requires_exhausted_eligible_retries() {
        let retry = RetryPolicy::Retry {
            partial_output: false,
        };
        let mut budget = RetryBudget::default();
        budget.observe("turn", 1);
        assert!(!budget.begin_trim(true, 2, retry));
        budget.used = 2;
        assert!(!budget.begin_trim(false, 2, retry));
        assert!(!budget.begin_trim(true, 0, retry));
        assert!(!budget.begin_trim(true, 2, RetryPolicy::Ineligible));
        assert!(!budget.begin_trim(true, 2, RetryPolicy::Deferred(Duration::from_secs(601))));
        assert!(budget.begin_trim(true, 2, retry));
        budget.succeeded();
        budget.used = 2;
        assert!(!budget.begin_trim(true, 2, retry));
        budget.observe("turn", 1);
        assert!(!budget.begin_trim(true, 2, retry));
        budget.observe("turn", 2);
        budget.used = 2;
        assert!(budget.begin_trim(true, 2, retry));
        budget.observe("other", 2);
        budget.used = 2;
        assert!(budget.begin_trim(true, 2, retry));
    }
}
