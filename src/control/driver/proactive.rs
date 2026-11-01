//! Proactive threshold compaction (#125): only the trigger decision lives here.
//! The argument-free compact path and its safe boundaries are reused as-is.
use super::*;

pub(super) const REASON: &str = "proactive_threshold";

/// Ephemeral per-Run budget. One `drive` call owns one Run, so a local value
/// suffices: after one non-adopted proactive attempt the Run never tries again.
/// Adopted attempts need a newer reported usage before another evaluation
/// (`next_request_pending` is true until a new attempt completes).
#[derive(Default)]
pub(super) struct ProactiveBudget {
    failed: bool,
}

struct Evidence {
    attempt: String,
    input_tokens: u64,
}

/// Called at the request boundary while the Thread boundary guard is held and
/// before the next attempt is prepared. Returns true when compaction ran (any
/// outcome); the caller then re-reads the Turn snapshot because the replay
/// lease was released and the selected path may have changed.
pub(super) async fn maybe_compact(
    control: &Arc<Control>,
    snapshot: &mut Snapshot,
    slot: &DriverSlot,
    budget: &mut ProactiveBudget,
) -> Result<bool, ApiError> {
    if budget.failed {
        return Ok(false);
    }
    let config = &control.store.config;
    let threshold = config
        .compaction
        .auto_at_tokens_for(config.connections.get(&snapshot.profile));
    if threshold == 0 {
        return Ok(false);
    }
    let thread = snapshot.thread.clone();
    let evidence = control
        .store
        .database
        .call(move |db| {
            let raw = crate::compaction::context_usage::read_context_usage_tx(db, &thread, None)?;
            // Only a provider-reported count triggers; unknown usage never does,
            // and usage measured before an adopted replacement is stale.
            Ok(raw
                .latest_reported_input()
                .and_then(|(attempt, tokens, stale)| {
                    tokens
                        .filter(|tokens| !stale && *tokens >= threshold)
                        .map(|input_tokens| Evidence {
                            attempt: attempt.to_owned(),
                            input_tokens,
                        })
                }))
        })
        .await
        .map_err(storage_error);
    let evidence = match evidence {
        Ok(Some(evidence)) => evidence,
        Ok(None) => return Ok(false),
        Err(error) if error.is_storage_failure() => return Err(error),
        // A non-storage read refusal is not evidence; never trigger on it.
        Err(_) => return Ok(false),
    };
    // Do not pin the replay frames while summary requests are in flight.
    snapshot.input.take();
    let mut observation = match super::super::compact::argument_free_compact(
        &control.compactor,
        &snapshot.thread,
        slot.stop.clone(),
    )
    .await
    {
        Ok(result) => {
            json!({"outcome":if result.no_target {"no_safe_reduction"} else {"adopted"},"summary":result})
        }
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(error) => json!({"outcome":"rejected","error":error}),
    };
    let adopted = observation["outcome"] == "adopted";
    if !adopted {
        budget.failed = true;
    }
    observation["reason"] = json!(REASON);
    observation["trigger"] = json!({
        "auto_at_tokens": threshold,
        "input_tokens": evidence.input_tokens,
        "run_id": snapshot.run,
    });
    // Record on the attempt whose reported usage triggered this decision, in the
    // existing automatic-compaction field and partial index (no new table).
    let body = observation.to_string();
    let attempt = evidence.attempt;
    control
        .store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim_attempt',json(?1)) WHERE id=?2 AND (error_json IS NULL OR json_valid(error_json))",
                params![body, attempt],
            )?;
            Ok(())
        })
        .await
        .map_err(storage_error)?;
    control.changed(&snapshot.thread);
    Ok(true)
}
