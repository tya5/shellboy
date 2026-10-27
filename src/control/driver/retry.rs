//! Consecutive conversation retries; the HTTP client never resends a request.
use super::*;
use std::time::Duration;

const MAX_RETRY_AFTER: Duration = Duration::from_secs(600);

#[derive(Default)]
pub(super) struct RetryBudget {
    turn: String,
    generation: i64,
    used: u32,
}
impl RetryBudget {
    pub(super) fn observe(&mut self, turn: &str, generation: i64) {
        if self.turn != turn || self.generation != generation {
            self.turn = turn.into();
            self.generation = generation;
            self.used = 0;
        }
    }
    /// Reset only after the completed response has been durably adopted.
    pub(super) fn succeeded(&mut self) {
        self.used = 0;
    }
    fn delay(
        &self,
        limit: u32,
        cap: Duration,
        retry: &model::http::ResponseRetry,
    ) -> Option<Duration> {
        if self.used >= limit {
            return None;
        }
        if let Some(delay) = retry.retry_after {
            return (delay <= MAX_RETRY_AFTER).then_some(delay);
        }
        Some(Duration::from_millis(1_000u64 << self.used.min(20)).min(cap))
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
    // Suppression is a per-connection policy, not a protocol/error reclassification.
    if !connection.response_retry_partial
        && failure
            .retry
            .as_ref()
            .is_some_and(|retry| retry.partial_output)
    {
        failure.retry = None;
    }
    let delay = failure
        .retry
        .as_ref()
        .filter(|_| !failure.error.is_context_limit())
        .and_then(|retry| {
            budget.delay(
                limit,
                Duration::from_millis(connection.response_retry_max_delay_ms),
                retry,
            )
        });
    if let Some(delay) = delay {
        let partial = failure
            .retry
            .as_ref()
            .is_some_and(|retry| retry.partial_output);
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
    if let Some(delay) = failure.retry.as_ref().and_then(|retry| retry.retry_after)
        && delay > MAX_RETRY_AFTER
    {
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
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_budget_resets_only_after_adoption_or_turn_generation_change() {
        let mut budget = RetryBudget::default();
        let hint = model::http::ResponseRetry {
            retry_after: None,
            partial_output: false,
        };
        budget.observe("t", 1);
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            assert_eq!(
                budget.delay(8, Duration::from_secs(60), &hint),
                Some(Duration::from_secs(seconds))
            );
            budget.used += 1;
            budget.observe("t", 1);
        }
        assert_eq!(budget.delay(8, Duration::from_secs(60), &hint), None);
        budget.succeeded();
        assert_eq!(
            budget.delay(8, Duration::from_secs(60), &hint),
            Some(Duration::from_secs(1))
        );
        budget.used = 8;
        budget.observe("t", 2);
        assert_eq!(budget.used, 0);
        assert_eq!(budget.delay(0, Duration::from_secs(60), &hint), None);
        budget.used = 8;
        budget.observe("other", 2);
        assert_eq!(budget.used, 0);
    }
    #[test]
    fn server_wait_is_never_shortened_and_longer_than_ten_minutes_is_deferred() {
        let budget = RetryBudget::default();
        for seconds in [0, 1, 120, 600, 601] {
            let hint = model::http::ResponseRetry {
                retry_after: Some(Duration::from_secs(seconds)),
                partial_output: false,
            };
            assert_eq!(
                budget.delay(8, Duration::from_millis(10), &hint),
                (seconds <= 600).then(|| Duration::from_secs(seconds))
            );
        }
    }
}
