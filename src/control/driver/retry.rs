//! Bounded conversation retries; the HTTP client never resends a request.
use super::*;
use std::time::Duration;

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
    fn delay(
        &self,
        limit: u32,
        cap: Duration,
        retry: &model::http::ResponseRetry,
    ) -> Option<Duration> {
        (self.used < limit).then(|| {
            retry
                .retry_after
                .unwrap_or_else(|| Duration::from_millis(1_000u64 << self.used.min(5)))
                .min(cap)
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn retry_or_recover(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    attempt: String,
    failure: model::http::AttemptFailure,
    budget: &mut RetryBudget,
    overflow: &mut OverflowBudget,
    boundary_guard: tokio::sync::MutexGuard<'_, ()>,
) -> Result<(), ApiError> {
    let connection = &control.store.config.connections[&snapshot.profile];
    let limit = connection.response_max_retries;
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
        let body = json!({"error":failure.error,"partial_text":failure.partial_text,
            "partial_reasoning":failure.partial_reasoning,"completed_items":failure.completed_items,
            "raw_response":failure.raw_response,"attempt_id":attempt,
            "automatic_retry":{"number":budget.used+1,"limit":limit,
                "delay_ms":delay.as_millis() as u64,"possible_duplicate_charge":true}});
        let saved_attempt = attempt.clone();
        let transition = control
            .store
            .database
            .call(move |db| super::super::state::fail_attempt(db, &saved_attempt, &body, &now()))
            .await
            .map_err(storage_error)?;
        if transition == super::super::state::AttemptTransition::Obsolete {
            return Ok(());
        }
        budget.used += 1;
        control.changed(&snapshot.thread);
        // Release admission only for backoff. Existing overflow/recovery keeps
        // its original boundary; the next loop revalidates ordinary admission.
        drop(boundary_guard);
        tokio::select! {
            biased;
            _ = slot.stop.cancelled() => {},
            _ = tokio::time::sleep(delay) => {},
        }
        return Ok(());
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
    fn budget_is_shared_until_turn_or_generation_changes_and_delays_are_bounded() {
        let mut budget = RetryBudget::default();
        let hint = model::http::ResponseRetry { retry_after: None };
        budget.observe("t", 1);
        assert_eq!(
            budget.delay(2, Duration::from_secs(30), &hint),
            Some(Duration::from_secs(1))
        );
        budget.used += 1;
        budget.observe("t", 1);
        assert_eq!(
            budget.delay(2, Duration::from_secs(30), &hint),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            budget.delay(2, Duration::from_millis(10), &hint),
            Some(Duration::from_millis(10))
        );
        budget.used += 1;
        assert_eq!(budget.delay(2, Duration::from_secs(30), &hint), None);
        budget.observe("t", 2);
        assert_eq!(budget.used, 0);
        assert_eq!(budget.delay(0, Duration::from_secs(30), &hint), None);
        budget.used = 2;
        budget.observe("next", 2);
        assert_eq!(budget.used, 0);
    }
}
