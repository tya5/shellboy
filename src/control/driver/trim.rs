//! One deterministic maximum reduction before POST or at the first large-input failure. The
//! Compactor owns sizing, safe ranges, provenance and atomic path adoption.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn try_trim(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    attempt: &str,
    failure: &mut model::http::AttemptFailure,
    hint: Option<u64>,
    retries: u32,
    limit: u32,
    not_applied: &mut Option<Value>,
) -> Result<bool, ApiError> {
    if slot.stop.is_cancelled() {
        return Ok(false);
    }
    let result = match control
        .compactor
        .trim_disconnect(&snapshot.thread, hint, slot.stop.clone())
        .await
    {
        Ok(result) => result,
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(error) => {
            *not_applied = Some(json!({"outcome":"rejected","error":error}));
            failure
                .error
                .message
                .push_str(&format!("; automatic context trim not adopted: {error}"));
            return Ok(false);
        }
    };
    if result.compact.no_target {
        let below_activation = result.before_tokens <= hint.unwrap_or(75_000);
        let (outcome, explanation) = if below_activation {
            (
                "below_activation_threshold",
                "estimated input does not exceed activation threshold",
            )
        } else {
            ("no_safe_reduction", "no safe groups outside protected work")
        };
        *not_applied = Some(json!({"outcome":outcome,"result":result}));
        failure.error.message.push_str(&format!(
            "; automatic context trim not adopted: {explanation}"
        ));
        return Ok(false);
    }
    let body = json!({
        "error":failure.error,
        "partial_text":failure.partial_text,
        "partial_reasoning":failure.partial_reasoning,
        "completed_items":failure.completed_items,
        "raw_response":failure.raw_response,
        "attempt_id":attempt,
        "automatic_trim_trigger":{"retries_used":retries,"retry_limit":limit,
            "retry_after_ms":failure.facts.retry_after.map(|delay| delay.as_millis().min(u64::MAX as u128) as u64)},
        "automatic_trim":result,
    });
    let partial = failure.facts.received == model::http::ResponseReceived::Output;
    let saved_attempt = attempt.to_owned();
    let turn = snapshot.turn.clone();
    let transition = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let transition = super::super::state::fail_attempt(&tx, &saved_attempt, &body, &now())?;
            if transition == super::super::state::AttemptTransition::Applied && partial {
                save_received_failure(&tx, &turn, &body)?;
            }
            tx.commit()?;
            Ok(transition)
        })
        .await
        .map_err(storage_error)?;
    // Obsolete means stop/generation change won. Do not renew its budget.
    if transition != super::super::state::AttemptTransition::Applied {
        return Ok(false);
    }
    control.changed(&snapshot.thread);
    Ok(true)
}

/// Explicit hint only: release the caller's replay before invoking this helper.
/// The caller reloads the same turn snapshot when no path change was adopted.
pub(super) async fn before_post(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    budget: &mut retry::RetryBudget,
) -> Result<bool, ApiError> {
    let connection = &control.store.config.connections[&snapshot.profile];
    let Some(hint) = connection.compact_hint_tokens else {
        return Ok(false);
    };
    if !budget.trim_available(connection.auto_trim_on_disconnect) {
        return Ok(false);
    }
    let result = control
        .compactor
        .trim_disconnect(&snapshot.thread, Some(hint), slot.stop.clone())
        .await?;
    if result.before_tokens <= hint {
        return Ok(false);
    }
    if result.compact.no_target {
        return Ok(false);
    }
    budget.consume_trim();
    control.changed(&snapshot.thread);
    Ok(true)
}
