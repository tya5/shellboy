//! Ephemeral per-Turn/generation budget; durable state remains in SQLite.
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
    pub(super) fn unlock_current(&mut self) {
        self.stage = 2;
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn retry_or_recover(
    control: &std::sync::Arc<super::Control>,
    snapshot: &super::Snapshot,
    slot: &super::DriverSlot,
    attempt: String,
    failure: super::model::http::AttemptFailure,
    overflow: &mut OverflowBudget,
    automatic_retries: u32,
    retry_limit: u32,
    retry_policy: super::retry::RetryPolicy,
    trim_not_applied: Option<serde_json::Value>,
) -> Result<(), super::ApiError> {
    use super::*;
    let thread = &snapshot.thread;

    if failure.error.is_context_limit() && overflow.available() {
        let attempt_id = attempt.clone();
        let body = json!({"error":failure.error,"partial_text":failure.partial_text,"partial_reasoning":failure.partial_reasoning,"attempt_id":attempt,"completed_items":failure.completed_items,"raw_response":failure.raw_response});
        let transition = control
            .store
            .database
            .call(move |db| crate::control::state::fail_attempt(db, &attempt_id, &body, &now()))
            .await
            .map_err(storage_error)?;
        if transition == crate::control::state::AttemptTransition::Obsolete {
            return Ok(());
        }
        let reduction =
            if overflow.begin().expect("available overflow budget") == Reduction::OldGroups {
                let spill = control
                    .compactor
                    .spill_cancellable(thread, false, slot.stop.clone())
                    .await;
                match spill {
                    Ok(ref result) if result.no_target => {
                        overflow.unlock_current();
                        spill_and_summarize(&control.compactor, thread, slot.stop.clone()).await
                    }
                    other => other,
                }
            } else {
                spill_and_summarize(&control.compactor, thread, slot.stop.clone()).await
            };
        match reduction {
            Ok(result) if !result.no_target => {
                control.changed(thread);
                return Ok(());
            }
            Ok(_) => {}
            Err(error) => {
                return fail_turn(control, snapshot, error).await;
            }
        }
    }
    let deferred = if let super::retry::RetryPolicy::Deferred(delay) = retry_policy {
        Some(delay)
    } else {
        None
    };
    let exhausted = matches!(retry_policy, super::retry::RetryPolicy::Retry { .. })
        && automatic_retries >= retry_limit
        && retry_limit > 0;
    let message = if exhausted {
        format!(
            "{} (automatic retries exhausted: {automatic_retries}/{retry_limit})",
            failure.error
        )
    } else {
        failure.error.to_string()
    };
    let error=ApiError::new(ErrorCode::IoFailed,message).hint("Inspect the incomplete response and explicitly retry this Run; tools already saved are not re-executed");
    let mut saved = json!({"error":failure.error,"partial_text":failure.partial_text,"partial_reasoning":failure.partial_reasoning,"attempt_id":attempt,"completed_items":failure.completed_items,"raw_response":failure.raw_response});
    if let Some(observation) = trim_not_applied {
        saved["automatic_trim_attempt"] = observation;
    }
    if let Some(delay) = deferred {
        saved["automatic_retry_deferred"] = json!({"retry_after_ms":delay.as_millis().min(u64::MAX as u128) as u64,"maximum_automatic_wait_ms":600_000});
    }
    if exhausted {
        saved["automatic_retry_exhausted"] =
            json!({"retries":automatic_retries,"limit":retry_limit});
    }
    recover(control, snapshot, error.clone(), Some((attempt, saved))).await?;
    Err(error)
}
#[cfg(test)]
mod tests {
    use super::*;
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
