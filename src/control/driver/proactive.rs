//! Proactive threshold compaction (#125): only the trigger decision lives here.
//! Uses ordinary safe boundaries, recording partial spill adoption separately.
use super::*;

pub(super) const REASON: &str = "proactive_threshold";

/// Ephemeral per-Run budget. One `drive` call owns one Run, so a local value
/// suffices. Non-adopted attempts require measured growth before retrying.
/// Adopted attempts need a newer reported usage before another evaluation
/// (`next_request_pending` is true until a new attempt completes).
#[derive(Default)]
pub(super) struct ProactiveBudget {
    last_non_adopted_input: Option<u64>,
}

impl ProactiveBudget {
    fn permits(&self, input_tokens: u64, threshold: u64) -> bool {
        self.last_non_adopted_input.is_none_or(|previous| {
            input_tokens
                .checked_sub(previous)
                .is_some_and(|growth| growth >= threshold.div_ceil(10).max(1))
        })
    }
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
    if !budget.permits(evidence.input_tokens, threshold) {
        return Ok(false);
    }
    // Do not pin the replay frames while summary requests are in flight.
    snapshot.input.take();
    let spill = control
        .compactor
        .spill_reducing_cancellable(&snapshot.thread, slot.stop.clone())
        .await;
    let mut observation = match spill {
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(error) => json!({"outcome":"rejected","error":error}),
        Ok(spill) => match control
            .compactor
            .summarize(&snapshot.thread, false, slot.stop.clone())
            .await
        {
            Ok(mut summary) => {
                summary.spilled_groups += spill.spilled_groups;
                summary.source_nodes += spill.source_nodes;
                summary.original_node_ids.extend(spill.original_node_ids);
                summary.no_target = summary.no_target && spill.no_target;
                json!({"outcome":if summary.no_target {"no_safe_reduction"} else {"adopted"},"summary":summary})
            }
            Err(error) if error.is_storage_failure() => return Err(error),
            Err(error) if !spill.no_target && !slot.stop.is_cancelled() => {
                let mut summary = json!(spill);
                summary["summary_rejected"] = json!(true);
                if let Some(details) = error.details.as_ref().and_then(Value::as_object) {
                    for field in [
                        "summary_usage",
                        "source_characters",
                        "summary_characters",
                        "summary_limit",
                        "model_requests",
                        "map_requests",
                        "reduce_requests",
                    ] {
                        if let Some(value) = details.get(field) {
                            summary[field] = value.clone();
                        }
                    }
                }
                json!({"outcome":"adopted","summary":summary,"summary_error":error})
            }
            Err(error) => json!({"outcome":"rejected","error":error}),
        },
    };
    let adopted = observation["outcome"] == "adopted";
    let summary_rejected = observation.get("summary_error").is_some();
    budget.last_non_adopted_input = (!adopted || summary_rejected).then_some(evidence.input_tokens);
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

#[cfg(test)]
mod tests {
    use super::ProactiveBudget;

    #[test]
    fn retry_growth_rounds_up_and_cannot_overflow() {
        let budget = ProactiveBudget {
            last_non_adopted_input: Some(200),
        };
        for count in [0, 199, 200, 210] {
            assert!(!budget.permits(count, 101));
        }
        assert!(budget.permits(211, 101));
        assert!(!budget.permits(200, 1));
        assert!(budget.permits(201, 1));
        let near_max = ProactiveBudget {
            last_non_adopted_input: Some(u64::MAX - 1),
        };
        assert!(!near_max.permits(u64::MAX, 100));
        assert!(near_max.permits(u64::MAX, 1));
        let max = ProactiveBudget {
            last_non_adopted_input: Some(u64::MAX),
        };
        assert!(!max.permits(u64::MAX, 1));
        assert!(!budget.permits(u64::MAX / 10, u64::MAX));
        assert!(ProactiveBudget::default().permits(200, 100));
    }
}
