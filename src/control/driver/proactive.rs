//! Proactive threshold compaction and measured retry policy (#125, #153).
//! Uses ordinary safe boundaries, recording partial spill adoption separately.
use super::*;

#[cfg(test)]
mod fault_tests;
mod target;

pub(super) const REASON: &str = "proactive_threshold";

/// Loaded lazily from durable Thread metadata; unlike token growth, the
/// witness changes only when eligible saved source facts may have changed.
#[derive(Default)]
pub(super) struct ProactiveBudget {
    loaded: bool,
    guard: Option<super::proactive_guard::Guard>,
    status_context: Option<(String, String)>,
    below_context: Option<(String, String)>,
    /// #182: the previous attempt in this Run adopted the ordinary prefix
    /// summary; if a fresh measurement is still at or above the threshold,
    /// the active-Turn summary goes first in the next attempt.
    prefix_adopted: bool,
}

enum Measurement {
    BelowThreshold,
    Ready(Evidence),
}

struct Evidence {
    attempt: String,
    input_tokens: u64,
    input_characters: Option<u64>,
    insufficient_prefix: bool,
    prior_turn_prefix: bool,
    insufficient_spill: bool,
    exhausted_active: bool,
    baseline_marks: Option<Vec<String>>,
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
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
) -> Result<bool, ApiError> {
    let config = &control.store.config;
    let threshold = config
        .compaction
        .auto_at_tokens_for(config.connections.get(&snapshot.profile));
    if threshold == 0 {
        if budget.status_context.take().is_some() {
            super::proactive_guard::hide_status(control, &snapshot.thread).await?;
        }
        return Ok(false);
    }
    use super::proactive_guard::{self as guard, Guard, Reason};
    if !budget.loaded {
        let (saved, has_status) = guard::load(control, &snapshot.thread).await?;
        budget.guard = saved;
        // The status may belong to a previous Run. A sentinel makes the
        // below-threshold path clear it, or the active path refresh it.
        budget.status_context = has_status.then(|| (String::new(), String::new()));
        budget.loaded = true;
    }
    if let Some(mut refreshed) = budget.guard.clone()
        && refreshed.refresh_after_adoption(snapshot)
    {
        let reason = refreshed.reason;
        if guard::save(
            control,
            snapshot,
            snapshot.selection as u64,
            &refreshed,
            reason,
        )
        .await?
        {
            budget.guard = Some(refreshed);
            budget.status_context = Some((snapshot.run.clone(), snapshot.turn.clone()));
        }
    }
    let thread = snapshot.thread.clone();
    let current_turn = snapshot.turn.clone();
    let evidence = control
        .store
        .database
        .call(move |db| {
            let raw = crate::compaction::context_usage::read_context_usage_tx(db, &thread, None)?;
            // Only a provider-reported count triggers; unknown usage never does,
            // and usage measured before an adopted replacement is stale.
            let Some((attempt, Some(input_tokens), false)) = raw.latest_reported_input() else {
                return Ok(None);
            };
            let target_key = crate::compaction::CompactionKeys::new(&thread).automatic_target();
            let pending: Option<String> = db.query_row("SELECT value FROM app_meta WHERE key=?1 AND octet_length(value)<=1048576", [&target_key], |row| row.get(0)).optional()?;
            let mut insufficient_prefix = false;
            let mut prior_turn_prefix = false;
            let mut insufficient_spill = false;
            let mut exhausted_current_turn = false;
            let mut baseline_marks = None;
            if let Some(mut pending) = pending.and_then(|s| serde_json::from_str::<Value>(&s).ok())
                && let (Some(previous), Some(target)) = (pending["attempt"].as_str().map(str::to_owned), pending["target_tokens"].as_u64())
                && previous != attempt
            {
                let same_model: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM model_attempt old JOIN model_attempt new ON old.profile_id=new.profile_id AND old.model_id=new.model_id WHERE old.id=?1 AND new.id=?2)", params![previous,attempt], |row| row.get(0))?;
                if !same_model || target != ((threshold as u128 * 4) / 5) as u64 {
                    db.execute("DELETE FROM app_meta WHERE key=?1", [&target_key])?;
                } else {
                    if pending["verified"].is_null() {
                        let insufficient = target::verify(target, input_tokens) == target::Verification::Insufficient;
                        let outcome = if insufficient {"insufficient"} else {"met"};
                        let verification = json!({"attempt_id":attempt,"input_tokens":input_tokens,"target_tokens":target,"outcome":outcome}).to_string();
                        pending["verified"] = json!(outcome);
                        let tx = db.transaction()?;
                        tx.execute("UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.target_verification',json(?1)) WHERE id=?2 AND json_valid(error_json)", params![verification,previous])?;
                        tx.execute("UPDATE app_meta SET value=?2 WHERE key=?1", params![target_key,pending.to_string()])?;
                        tx.commit()?;
                    }
                    if pending["verified"] == "met" {
                        db.execute("DELETE FROM app_meta WHERE key=?1", [&target_key])?;
                    } else if pending["scope"] == "prefix" {
                        if pending["turn"] == current_turn {
                            insufficient_prefix = true;
                        } else {
                            prior_turn_prefix = true;
                        }
                    } else if pending["scope"] == "spill" {
                        insufficient_spill = true;
                    } else if pending["scope"] == "active_turn" && pending["turn"] == current_turn {
                        exhausted_current_turn = true;
                        baseline_marks = serde_json::from_value(pending["marks"].clone()).ok();
                    }
                }
            }
            if input_tokens < threshold && !insufficient_prefix && !prior_turn_prefix && !insufficient_spill && !exhausted_current_turn {
                return Ok(Some(Measurement::BelowThreshold));
            }
            let input_characters = db.query_row(
                "SELECT CASE WHEN json_valid(input_manifest_json) THEN CASE WHEN json_type(input_manifest_json,'$.input_characters')='integer' THEN json_extract(input_manifest_json,'$.input_characters') END END FROM model_attempt WHERE id=?1",
                [attempt], |row| row.get::<_, Option<i64>>(0),
            )?.and_then(|n| u64::try_from(n).ok());
            Ok(Some(Measurement::Ready(Evidence { attempt: attempt.to_owned(), input_tokens, input_characters, insufficient_prefix, prior_turn_prefix, insufficient_spill, exhausted_active: exhausted_current_turn, baseline_marks })))
        })
        .await
        .map_err(storage_error);
    let evidence = match evidence {
        Ok(Some(Measurement::Ready(evidence))) => evidence,
        Ok(Some(Measurement::BelowThreshold)) => {
            // A fresh measurement below the threshold ends the #182
            // "still at or above after the prefix summary" condition.
            budget.prefix_adopted = false;
            if budget.status_context.take().is_some() {
                super::proactive_guard::hide_status(control, &snapshot.thread).await?;
            }
            let context = (snapshot.run.clone(), snapshot.turn.clone());
            if budget.below_context.as_ref() != Some(&context) {
                control.diagnostics.record(
                    crate::diagnostics::DiagnosticEvent::AutomaticCompactionBelowThreshold,
                    crate::diagnostics::DiagnosticIds {
                        thread_id: Some(snapshot.thread.clone()),
                        run_id: Some(snapshot.run.clone()),
                        turn_id: Some(snapshot.turn.clone()),
                        ..Default::default()
                    },
                );
                budget.below_context = Some(context);
            }
            return Ok(false);
        }
        Ok(None) => {
            if budget.status_context.take().is_some() {
                super::proactive_guard::hide_status(control, &snapshot.thread).await?;
            }
            return Ok(false);
        }
        Err(error) if error.is_storage_failure() => return Err(error),
        // A non-storage read refusal is not evidence; never trigger on it.
        Err(_) => {
            if budget.status_context.take().is_some() {
                super::proactive_guard::hide_status(control, &snapshot.thread).await?;
            }
            return Ok(false);
        }
    };
    if let Some(reason) = budget
        .guard
        .as_ref()
        .filter(|guard| guard.unchanged(snapshot))
        .map(|guard| guard.reason)
    {
        let context = (snapshot.run.clone(), snapshot.turn.clone());
        if budget.status_context.as_ref() != Some(&context)
            && guard::show_status(control, snapshot, reason).await?
        {
            budget.status_context = Some(context);
        }
        return Ok(false);
    }
    // Evaluate only after saved source evidence changes. The forecast is
    // bounded and read-only; a coverage limit is not evidence of no targets.
    let candidates = guard::candidates(control, &snapshot.thread, snapshot.input.clone()).await?;
    if candidates
        .as_ref()
        .is_some_and(|candidate| candidate.revision != snapshot.selection as u64)
    {
        return Ok(false);
    }
    let open_loop = control.compactor.tool_loop_open(&snapshot.thread).await?;
    let calibrated = evidence
        .input_characters
        .and_then(|chars| target::plan(threshold, evidence.input_tokens, chars))
        .is_some();
    let marks = candidates.as_ref().map(|candidate| {
        candidate
            .marks
            .iter()
            .filter(|mark| {
                calibrated
                    || (!mark.starts_with("partial:") && !(open_loop && mark.starts_with("spill:")))
            })
            .cloned()
            .collect::<Vec<_>>()
    });
    let protected = candidates
        .as_ref()
        .is_some_and(|candidate| candidate.mutable_protection);
    let no_target = marks.as_ref().is_some_and(Vec::is_empty);
    let has_kind = |kind: &str| match &budget.guard {
        Some(previous) => previous.has_new_kind(marks.as_deref(), kind),
        None => marks
            .as_ref()
            .is_none_or(|marks| marks.iter().any(|mark| mark.starts_with(kind))),
    };
    let widened_active = evidence.exhausted_active
        && match (&evidence.baseline_marks, &marks) {
            (Some(previous), Some(current)) => {
                guard::has_new_marks(previous, current, "middle:")
                    || (calibrated && guard::has_new_marks(previous, current, "partial:"))
            }
            _ => false,
        };
    let spill_new = (calibrated
        || (!evidence.insufficient_prefix
            && !evidence.insufficient_spill
            && !evidence.exhausted_active))
        && has_kind("spill:");
    let summary_new = !evidence.insufficient_prefix
        && has_kind("summary:")
        && !(marks.is_none()
            && budget
                .guard
                .as_ref()
                .is_some_and(|guard| guard.blocks_unknown_summary(snapshot)));
    let middle_new = if evidence.exhausted_active {
        widened_active
    } else {
        has_kind("middle:") || (calibrated && has_kind("partial:"))
    };
    let active_spill_new = calibrated && spill_new;
    let any_summary_new = middle_new || summary_new;
    let no_growth = !spill_new && !any_summary_new;
    let ids = crate::diagnostics::DiagnosticIds {
        thread_id: Some(snapshot.thread.clone()),
        run_id: Some(snapshot.run.clone()),
        turn_id: Some(snapshot.turn.clone()),
        ..Default::default()
    };
    if candidates.is_none() {
        let counts = budget.guard.as_ref().map_or((0, 0), |guard| guard.counts());
        control.diagnostics.record_automatic_compaction(
            crate::diagnostics::DiagnosticEvent::AutomaticCompactionForecastLimited,
            ids.clone(),
            counts.0,
            counts.1,
        );
    }
    if no_target || no_growth {
        let reason = if no_target {
            Reason::NoTarget
        } else {
            Reason::NoNewTarget
        };
        let context = (snapshot.run.clone(), snapshot.turn.clone());
        if budget
            .guard
            .as_ref()
            .is_some_and(|previous| previous.reason == reason && previous.same_witness(snapshot))
        {
            if budget.status_context.as_ref() != Some(&context)
                && guard::show_status(control, snapshot, reason).await?
            {
                budget.status_context = Some(context);
            }
            return Ok(false);
        }
        if let Some(current) = Guard::new(snapshot, marks, protected, reason, budget.guard.as_ref())
            && guard::save(
                control,
                snapshot,
                snapshot.selection as u64,
                &current,
                reason,
            )
            .await?
        {
            let counts = current.counts();
            budget.guard = Some(current);
            budget.status_context = Some(context);
            control.diagnostics.record_automatic_compaction(
                if no_target {
                    crate::diagnostics::DiagnosticEvent::AutomaticCompactionSkippedNoTarget
                } else {
                    crate::diagnostics::DiagnosticEvent::AutomaticCompactionSkippedNoNewTarget
                },
                ids,
                counts.0,
                counts.1,
            );
        }
        return Ok(false);
    }
    let latest_tool_anchor = snapshot
        .input
        .as_ref()
        .and_then(|input| input.latest_tool_anchor())
        .map(str::to_owned);
    let preserve_guard_after_spill = !any_summary_new && budget.guard.is_some();
    let mut candidate_guard = Guard::new(
        snapshot,
        marks,
        protected,
        Reason::CoverageLimited,
        budget.guard.as_ref(),
    );
    let mut spill_counts = control
        .compactor
        .proactive_spill_counts(
            &snapshot.thread,
            open_loop
                && evidence
                    .input_characters
                    .and_then(|chars| target::plan(threshold, evidence.input_tokens, chars))
                    .is_none(),
            calibrated || spill_new,
        )
        .await?;
    let key = crate::compaction::CompactionKeys::new(&snapshot.thread).automatic_active();
    let witness = json!({"run_id": snapshot.run, "turn_id": snapshot.turn,
        "input_tokens": evidence.input_tokens, "threshold": threshold,
        "started_at": chrono::Utc::now().to_rfc3339()})
    .to_string();
    let (saved_key, saved_witness) = (key.clone(), witness.clone());
    let status_key = crate::compaction::CompactionKeys::new(&snapshot.thread).automatic_status();
    control.store.database.call(move |db| {
        let tx = db.transaction()?;
        tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![saved_key, saved_witness])?;
        tx.execute("DELETE FROM app_meta WHERE key=?1", [status_key])?;
        tx.commit()?;
        Ok(())
    }).await.map_err(storage_error)?;
    control.diagnostics.record(
        crate::diagnostics::DiagnosticEvent::AutomaticCompactionStarted,
        ids.clone(),
    );
    control.changed(&snapshot.thread);
    // Do not pin the replay frames while summary requests are in flight.
    snapshot.input.take();
    let target_plan = evidence
        .input_characters
        .and_then(|chars| target::plan(threshold, evidence.input_tokens, chars));
    let mut midturn = Value::Null;
    let prefer_active = target_plan.is_none()
        && (evidence.insufficient_prefix
            || (!evidence.prior_turn_prefix && std::mem::take(&mut budget.prefix_adopted)));
    if evidence.prior_turn_prefix {
        budget.prefix_adopted = false;
    }
    let mut summary_scope = "";
    let mut selection_shortfall = None;
    // At most one result per stage; keep provenance until the audit is durable.
    let mut committed = Vec::new();
    let mut stages = json!({"closed_spill":{"ran":false,"skipped":"not_reached"},"closed_summary":{"ran":false,"skipped":"not_reached"},"active_spill":{"ran":false,"skipped":"not_reached"},"active_summary":{"ran":false,"skipped":"not_reached"}});
    let result: Result<Value, ApiError> = async {
    let before_characters = if target_plan.is_some() {
        Some(control.compactor.proactive_history_characters(&snapshot.thread).await?)
    } else { None };
    let spill = if let Some(plan) = target_plan {
        control.compactor.spill_staged_cancellable(&snapshot.thread, false, plan.remove_characters, slot.stop.clone()).await
    } else if spill_new && !open_loop {
        control.compactor.spill_reducing_cancellable(&snapshot.thread, slot.stop.clone()).await
    } else {
        Ok(crate::compaction::CompactResult { no_target: true, ..Default::default() })
    };
    stages["closed_spill"] = json!({"ran":target_plan.is_some() || (spill_new && !open_loop), "remaining_before":target_plan.map(|p|p.remove_characters), "adopted_groups":spill.as_ref().ok().map_or(0, |r| r.spilled_groups), "outcome":match &spill { Ok(r) if r.no_target => "no_target", Ok(_) => "adopted", Err(_) => "rejected" }});
    if let Ok(spill) = &spill { committed.push(spill.clone()); }
    #[cfg(test)]
    if spill.as_ref().is_ok_and(|r| !r.no_target) {
        fault_tests::checkpoint(control, slot, "closed_spill").await?;
    }
    let after_spill = if target_plan.is_some() {
        Some(control.compactor.proactive_history_characters(&snapshot.thread).await?)
    } else { None };
    let remaining = target_plan.map(|plan| plan.remove_characters.saturating_sub(
        before_characters.unwrap_or(0).saturating_sub(after_spill.unwrap_or(0))));
    stages["closed_spill"] = json!({"ran":target_plan.is_some() || (spill_new && !open_loop), "removed_characters":before_characters.zip(after_spill).map(|(a,b)|a.saturating_sub(b)), "remaining_before":target_plan.map(|p|p.remove_characters), "remaining_after":remaining, "adopted_groups":spill.as_ref().ok().map_or(0, |r| r.spilled_groups), "outcome":match &spill { Ok(r) if r.no_target => "no_target", Ok(_) => "adopted", Err(_) => "rejected" }});
    if target_plan.is_none() && (!spill_new || open_loop) {
        stages["closed_spill"]["skipped"] = json!(if open_loop { "open_tool_loop" } else { "driver_gate" });
        stages["closed_spill"]["outcome"] = json!("skipped");
    }
    let observation = match spill {
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(error) => json!({"outcome":"rejected","error":error}),
        Ok(spill) if (!any_summary_new && !active_spill_new) || remaining == Some(0) => json!({
            "outcome": if spill.no_target { "no_safe_reduction" } else { "adopted" },
            "summary": spill,
            "summary_deferred": true,
        }),
        Ok(spill) => match summarize_once(control, &snapshot.thread, slot, Order { prefer_active, middle_new, active_spill_new, ordinary_new: summary_new }, &mut midturn, &mut summary_scope, remaining, &mut selection_shortfall, &mut stages, &mut committed, boundary).await
        {
            Ok(mut summary) => {
                if summary_scope == "prefix" && (middle_new || active_spill_new) && !slot.stop.is_cancelled()
                    && let (Some(plan), Some(before)) = (target_plan, before_characters)
                {
                    let after = control.compactor.proactive_history_characters(&snapshot.thread).await?;
                    if before.saturating_sub(after) < plan.remove_characters {
                        match active_after_spill(control, &snapshot.thread, slot, Some(plan.remove_characters.saturating_sub(before.saturating_sub(after))), middle_new, &mut stages, &mut committed, boundary).await {
                            Ok(active) if !active.no_target => {
                                summary.spilled_groups = summary.spilled_groups.saturating_add(active.spilled_groups);
                                summary.source_nodes = summary.source_nodes.saturating_add(active.source_nodes);
                                if active.summary_node_id.is_some() {
                                summary.created_parts = summary.created_parts.saturating_add(active.created_parts);
                                summary.reused_parts = summary.reused_parts.saturating_add(active.reused_parts);
                                summary.source_characters = summary.source_characters.zip(active.source_characters).map(|(a,b)| a.saturating_add(b));
                                summary.summary_characters = summary.summary_characters.zip(active.summary_characters).map(|(a,b)| a.saturating_add(b));
                                summary.summary_limit = summary.summary_limit.zip(active.summary_limit).map(|(a,b)| a.saturating_add(b));
                                summary.model_requests = summary.model_requests.zip(active.model_requests).map(|(a,b)| a.saturating_add(b));
                                if let (Some(total), Some(usage)) = (&mut summary.summary_usage, &active.summary_usage) {
                                    total.merge(usage);
                                } else {
                                    summary.summary_usage = None;
                                }
                                summary.summary_node_id = active.summary_node_id;
                                }
                                summary.original_node_ids.extend(active.original_node_ids);
                                summary.no_target = false;
                                midturn = json!({"scope":"active_turn", "reason":"target_shortfall"});
                                summary_scope = "active_turn";
                            }
                            Ok(_) => {}
                            Err(error) if error.is_storage_failure() || slot.stop.is_cancelled() => return Err(error),
                            Err(error) => {
                                summary.spilled_groups = summary.spilled_groups.saturating_add(spill.spilled_groups).saturating_add(stages["active_spill"]["adopted_groups"].as_u64().unwrap_or(0) as usize);
                                summary.source_nodes += spill.source_nodes;
                                summary.original_node_ids.extend(spill.original_node_ids);
                                return Ok(json!({"outcome":"adopted","summary":summary,"summary_error":error}));
                            }
                        }
                    }
                }
                summary.spilled_groups += spill.spilled_groups;
                summary.source_nodes += spill.source_nodes;
                summary.original_node_ids.extend(spill.original_node_ids);
                summary.no_target = summary.no_target && spill.no_target;
                json!({"outcome":if summary.no_target {"no_safe_reduction"} else {"adopted"},"summary":summary})
            }
            Err(error) if error.is_storage_failure() || slot.stop.is_cancelled() => return Err(error),
            Err(error) if !spill.no_target || stages["active_spill"]["outcome"] == "adopted" => {
                let mut summary = json!(spill);
                summary["spilled_groups"] = json!(summary["spilled_groups"].as_u64().unwrap_or(0).saturating_add(stages["active_spill"]["adopted_groups"].as_u64().unwrap_or(0)));
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
    Ok(observation)
    }.await;
    // A retiring owner must not remove a newer owner's witness.
    let cleanup = control
        .store
        .database
        .call(move |db| {
            db.execute(
                "DELETE FROM app_meta WHERE key=?1 AND value=?2",
                params![key, witness],
            )?;
            Ok(())
        })
        .await
        .map_err(storage_error);
    control.diagnostics.record(
        crate::diagnostics::DiagnosticEvent::AutomaticCompactionFinished,
        ids.clone(),
    );
    control.changed(&snapshot.thread);
    cleanup?;
    // Cancellation still fails the caller, but cannot undo committed path edits.
    let mut cancellation = None;
    let mut observation = match result {
        Err(error) if slot.stop.is_cancelled() && !error.is_storage_failure() => {
            let summary = committed_result(&committed);
            let observation = if summary.no_target {
                json!({"outcome":"rejected", "error":error, "cancelled":true})
            } else {
                let mut summary = json!(summary);
                summary["summary_rejected"] = json!(true);
                if let Some(details) = &error.details {
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
                json!({"outcome":"adopted", "summary":summary, "summary_error":error, "cancelled":true})
            };
            cancellation = Some(error);
            observation
        }
        Ok(observation) => observation,
        Err(error) => {
            spill_counts.spilled = stages["closed_spill"]["adopted_groups"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(
                    stages["active_spill"]["adopted_groups"]
                        .as_u64()
                        .unwrap_or(0),
                )
                .min(u32::MAX as u64) as u32;
            let body = json!({"outcome":"exception", "cancelled":slot.stop.is_cancelled(), "stages":stages, "spill_counts":spill_counts}).to_string();
            let attempt = evidence.attempt.clone();
            control.store.database.call(move |db| {
            db.execute("UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim_attempt',json(?1)) WHERE id=?2 AND (error_json IS NULL OR json_valid(error_json))", params![body,attempt])?;
            Ok(())
        }).await.map_err(storage_error)?;
            return Err(error);
        }
    };
    spill_counts.spilled = observation["summary"]["spilled_groups"]
        .as_u64()
        .unwrap_or(0)
        .max(
            stages["closed_spill"]["adopted_groups"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(
                    stages["active_spill"]["adopted_groups"]
                        .as_u64()
                        .unwrap_or(0),
                ),
        )
        .min(u32::MAX as u64) as u32;
    observation["spill_counts"] = json!(spill_counts);
    if stages["closed_spill"]["remaining_after"] == 0 {
        for stage in ["closed_summary", "active_spill", "active_summary"] {
            if stages[stage]["ran"] == false {
                stages[stage]["skipped"] = json!("target_met");
            }
        }
    } else if stages["closed_summary"]["remaining_after"] == 0 {
        for stage in ["active_spill", "active_summary"] {
            if stages[stage]["ran"] == false {
                stages[stage]["skipped"] = json!("target_met");
            }
        }
    }
    observation["stages"] = stages;
    observation["steps"] = json!({"spill_attempted": target_plan.is_some() || (spill_new && !open_loop), "spill_adopted": spill_counts.spilled > 0, "fresh_measurement": target_plan.is_some(), "summary_selection": !summary_scope.is_empty(), "summary_bundling": observation["summary"]["model_requests"].as_u64().is_some_and(|n| n > 0)});
    let adopted = observation["outcome"] == "adopted";
    let summary_rejected = observation.get("summary_error").is_some();
    let summary_deferred = observation["summary_deferred"] == true;
    budget.prefix_adopted = adopted && !summary_rejected && summary_scope == "prefix";
    if !adopted || summary_rejected || (summary_deferred && preserve_guard_after_spill) {
        let rejection = observation
            .get("summary_error")
            .or_else(|| observation.get("error"));
        let reason = if summary_deferred && adopted {
            budget
                .guard
                .as_ref()
                .map_or(Reason::NoNewTarget, |guard| guard.reason)
        } else {
            match rejection
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
            {
                Some("invalid_state") => Reason::RejectedInvalidState,
                Some(_) => Reason::RejectedOther,
                None if observation["outcome"] == "no_target" => Reason::NoTarget,
                None => Reason::NoNewTarget,
            }
        };
        let mut counts = (0, 0);
        if let Some(current) = &mut candidate_guard {
            let mut revision = snapshot.selection as u64;
            if adopted && (summary_rejected || summary_deferred) {
                let after = guard::candidates(control, &snapshot.thread, None).await?;
                revision = match &after {
                    Some(candidate) => candidate.revision,
                    None => {
                        let revision = guard::current_revision(control, &snapshot.thread).await?;
                        current.mark_refresh(revision);
                        if summary_rejected {
                            current.freeze_summary_for_current_turn(
                                guard::anchor_is_current_turn(
                                    control,
                                    latest_tool_anchor.clone(),
                                    &snapshot.turn,
                                )
                                .await?,
                            );
                        }
                        revision as u64
                    }
                };
                current.replace_candidates(
                    after.as_ref().map(|candidate| candidate.marks.to_vec()),
                    after
                        .as_ref()
                        .is_some_and(|candidate| candidate.mutable_protection),
                );
            }
            if summary_deferred && adopted {
                current.set_reason(reason);
            } else {
                current.record_reason(reason);
            }
            counts = current.counts();
            if guard::save(control, snapshot, revision, current, reason).await? {
                budget.guard = candidate_guard;
                budget.status_context = Some((snapshot.run.clone(), snapshot.turn.clone()));
            }
        }
        match reason {
            Reason::RejectedInvalidState if !summary_deferred => {
                control.diagnostics.record_automatic_compaction(
                    crate::diagnostics::DiagnosticEvent::AutomaticCompactionRejectedInvalidState,
                    ids.clone(),
                    counts.0,
                    counts.1,
                )
            }
            Reason::RejectedOther if !summary_deferred => {
                control.diagnostics.record_automatic_compaction(
                    crate::diagnostics::DiagnosticEvent::AutomaticCompactionRejectedOther,
                    ids.clone(),
                    counts.0,
                    counts.1,
                )
            }
            _ => {}
        }
    } else {
        guard::clear(control, &snapshot.thread).await?;
        budget.guard = None;
    }
    observation["reason"] = json!(REASON);
    if !midturn.is_null() {
        observation["midturn"] = midturn;
    }
    if let Some(shortfall) = selection_shortfall {
        observation["selection_shortfall_characters"] = json!(shortfall);
    }
    if let Some(plan) = target_plan {
        observation["target_plan"] = json!({"target_tokens":plan.target_tokens,"remove_characters":plan.remove_characters,"input_characters":evidence.input_characters});
    }
    observation["trigger"] = json!({
        "auto_at_tokens": threshold,
        "input_tokens": evidence.input_tokens,
        "run_id": snapshot.run,
    });
    // Record on the attempt whose reported usage triggered this decision, in the
    // existing automatic-compaction field and partial index (no new table).
    let baseline = if adopted && target_plan.is_some() && summary_scope == "active_turn" {
        guard::candidates(control, &snapshot.thread, None)
            .await
            .ok()
            .flatten()
            .map(|c| c.marks)
    } else {
        None
    };
    let pending_target = if adopted {
        target_plan.map(|plan| {
            let scope = if summary_scope.is_empty() { "spill" } else { summary_scope };
            let mut value = json!({"attempt":evidence.attempt,"target_tokens":plan.target_tokens,"scope":scope,"turn":snapshot.turn,"marks":baseline});
            if value.to_string().len() > 1_048_576 {
                value["marks"] = Value::Null;
            }
            value.to_string()
        })
    } else {
        None
    };
    let target_key = crate::compaction::CompactionKeys::new(&snapshot.thread).automatic_target();
    let body = observation.to_string();
    let attempt = evidence.attempt;
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            tx.execute(
                "UPDATE model_attempt SET error_json=json_set(COALESCE(error_json,'{}'),'$.automatic_trim_attempt',json(?1)) WHERE id=?2 AND (error_json IS NULL OR json_valid(error_json))",
                params![body, attempt],
            )?;
            if let Some(pending) = pending_target {
                tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![target_key,pending])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(storage_error)?;
    control.changed(&snapshot.thread);
    if let Some(error) = cancellation {
        return Err(error);
    }
    Ok(true)
}

fn committed_result(
    committed: &[crate::compaction::CompactResult],
) -> crate::compaction::CompactResult {
    let mut result = crate::compaction::CompactResult {
        no_target: true,
        ..Default::default()
    };
    for stage in committed.iter().filter(|stage| !stage.no_target) {
        result.no_target = false;
        result.spilled_groups += stage.spilled_groups;
        result.source_nodes += stage.source_nodes;
        result.created_parts += stage.created_parts;
        result.reused_parts += stage.reused_parts;
        result
            .original_node_ids
            .extend(stage.original_node_ids.iter().cloned());
        if stage.summary_node_id.is_some() {
            result.summary_node_id.clone_from(&stage.summary_node_id);
            result.summary_usage.clone_from(&stage.summary_usage);
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn active_after_spill(
    control: &Arc<Control>,
    thread: &str,
    slot: &DriverSlot,
    remaining: Option<u64>,
    allow_summary: bool,
    stages: &mut Value,
    committed: &mut Vec<crate::compaction::CompactResult>,
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
) -> Result<crate::compaction::CompactResult, ApiError> {
    let Some(need) = remaining else {
        return control
            .compactor
            .summarize_active_guarded(thread, slot.stop.clone(), Some(&mut *boundary))
            .await;
    };
    let before = control
        .compactor
        .proactive_history_characters(thread)
        .await?;
    stages["active_spill"] = json!({"ran":true,"remaining_before":need,"outcome":"started"});
    let spill = match control
        .compactor
        .spill_staged_cancellable(thread, true, need, slot.stop.clone())
        .await
    {
        Ok(spill) => spill,
        Err(error) => {
            stages["active_spill"]["outcome"] = json!(if slot.stop.is_cancelled() {
                "cancelled"
            } else {
                "rejected"
            });
            return Err(error);
        }
    };
    committed.push(spill.clone());
    #[cfg(test)]
    if !spill.no_target {
        fault_tests::checkpoint(control, slot, "active_spill").await?;
    }
    stages["active_spill"]["adopted_groups"] = json!(spill.spilled_groups);
    stages["active_spill"]["outcome"] = json!(if spill.no_target {
        "no_target"
    } else {
        "adopted"
    });
    let after = control
        .compactor
        .proactive_history_characters(thread)
        .await?;
    let removed = before.saturating_sub(after);
    let remaining = need.saturating_sub(removed);
    stages["active_spill"] = json!({"ran":true,"removed_characters":removed,"adopted_groups":spill.spilled_groups,"remaining_before":need,"remaining_after":remaining,"outcome":if spill.no_target {"no_target"} else {"adopted"}});
    if remaining == 0 {
        stages["active_summary"] = json!({"ran":false,"skipped":"target_met","remaining_before":0});
        return Ok(spill);
    }
    if !allow_summary {
        stages["active_summary"] =
            json!({"ran":false,"skipped":"no_new_target","remaining_before":remaining});
        return Ok(spill);
    }
    stages["active_summary"] = json!({"ran":true,"remaining_before":remaining,"outcome":"started"});
    let (mut summary, projected_shortfall) = match control
        .compactor
        .summarize_target_active_guarded(thread, remaining, slot.stop.clone(), Some(&mut *boundary))
        .await
    {
        Ok(summary) => summary,
        Err(error) => {
            stages["active_summary"]["outcome"] = json!(if slot.stop.is_cancelled() {
                "cancelled"
            } else {
                "rejected"
            });
            return Err(error);
        }
    };
    committed.push(summary.clone());
    #[cfg(test)]
    if !summary.no_target {
        fault_tests::checkpoint(control, slot, "active_summary").await?;
    }
    stages["active_summary"]["outcome"] = json!(if summary.no_target {
        "no_target"
    } else {
        "adopted"
    });
    let after_summary = control
        .compactor
        .proactive_history_characters(thread)
        .await?;
    let summary_removed = after.saturating_sub(after_summary);
    stages["active_summary"] = json!({"ran":true,"removed_characters":summary_removed,"remaining_before":remaining,"remaining_after":remaining.saturating_sub(summary_removed),"projected_shortfall":projected_shortfall,"outcome":if summary.no_target {"no_target"} else {"adopted"}});
    summary.spilled_groups += spill.spilled_groups;
    summary.source_nodes += spill.source_nodes;
    summary.original_node_ids.extend(spill.original_node_ids);
    summary.no_target &= spill.no_target;
    Ok(summary)
}

struct Order {
    prefer_active: bool,
    middle_new: bool,
    active_spill_new: bool,
    ordinary_new: bool,
}

/// Select one summary operation. #182 order: the ordinary summary of
/// earlier Turns first (oldest context matters least); the active-Turn
/// summary only when that prefix has nothing but earlier summaries, or when
/// the previous attempt adopted the prefix summary in this Turn and a fresh
/// measurement remains above the calibrated target or threshold (`prefer_active`).
#[allow(clippy::too_many_arguments)]
async fn summarize_once(
    control: &Arc<Control>,
    thread: &str,
    slot: &DriverSlot,
    order: Order,
    midturn: &mut Value,
    scope: &mut &'static str,
    remaining: Option<u64>,
    shortfall: &mut Option<u64>,
    stages: &mut Value,
    committed: &mut Vec<crate::compaction::CompactResult>,
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
) -> Result<crate::compaction::CompactResult, ApiError> {
    let compactor = &control.compactor;
    if !order.prefer_active && order.ordinary_new {
        let before = if remaining.is_some() {
            Some(compactor.proactive_history_characters(thread).await?)
        } else {
            None
        };
        stages["closed_summary"] =
            json!({"ran":true,"remaining_before":remaining,"outcome":"started"});
        let prefix = if let Some(target) = remaining {
            compactor
                .summarize_target_prefix_guarded(
                    thread,
                    target,
                    slot.stop.clone(),
                    Some(&mut *boundary),
                )
                .await
                .map(|(result, missing)| {
                    *shortfall = Some(missing);
                    result
                })
        } else {
            compactor
                .summarize_prefix_guarded(thread, slot.stop.clone(), Some(&mut *boundary))
                .await
        };
        if let Ok(summary) = &prefix {
            committed.push(summary.clone());
        }
        #[cfg(test)]
        if prefix.as_ref().is_ok_and(|r| !r.no_target) {
            fault_tests::checkpoint(control, slot, "closed_summary").await?;
        }
        stages["closed_summary"]["outcome"] = json!(match &prefix {
            Ok(r) if r.no_target => "no_target",
            Ok(_) => "adopted",
            Err(_) if slot.stop.is_cancelled() => "cancelled",
            Err(_) => "rejected",
        });
        // Do not replace a summary error with a later cancelled snapshot read.
        if prefix.is_err() {
            *midturn = json!({"skipped":"prefix_first"});
            *scope = "prefix";
            return prefix;
        }
        let after = if remaining.is_some() {
            Some(compactor.proactive_history_characters(thread).await?)
        } else {
            None
        };
        let removed = before.zip(after).map(|(a, b)| a.saturating_sub(b));
        stages["closed_summary"] = json!({"ran":true,"remaining_before":remaining,"remaining_after":remaining.zip(removed).map(|(n,r)|n.saturating_sub(r)),"removed_characters":removed,"projected_shortfall":*shortfall,"outcome":match &prefix { Ok(r) if r.no_target => "no_target", Ok(_) => "adopted", Err(_) if slot.stop.is_cancelled() => "cancelled", Err(_) => "rejected" }});
        if !matches!(&prefix, Ok(result) if result.no_target) {
            *midturn = json!({"skipped":"prefix_first"});
            *scope = "prefix";
            return prefix;
        }
    }
    if order.middle_new || order.active_spill_new {
        let remaining = stages["closed_summary"]["remaining_after"]
            .as_u64()
            .or(remaining);
        let active = active_after_spill(
            control,
            thread,
            slot,
            remaining,
            order.middle_new,
            stages,
            committed,
            boundary,
        )
        .await;
        if !matches!(&active, Ok(result) if result.no_target) {
            *midturn = json!({"scope":"active_turn"});
            *scope = "active_turn";
            return active;
        }
        *midturn = json!({"skipped":"no_target"});
    } else {
        *midturn = json!({"skipped":"no_new_target"});
    }
    if order.prefer_active && order.ordinary_new {
        *scope = "prefix";
        return if let Some(target) = remaining {
            compactor
                .summarize_target_prefix_guarded(
                    thread,
                    target,
                    slot.stop.clone(),
                    Some(&mut *boundary),
                )
                .await
                .map(|(result, missing)| {
                    *shortfall = Some(missing);
                    result
                })
        } else {
            compactor
                .summarize_prefix_guarded(thread, slot.stop.clone(), Some(&mut *boundary))
                .await
        };
    }
    Ok(crate::compaction::CompactResult {
        no_target: true,
        ..Default::default()
    })
}
