//! Proactive threshold compaction and measured retry policy (#125, #153).
//! Uses ordinary safe boundaries, recording partial spill adoption separately.
use super::*;

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
            let mut insufficient_spill = false;
            let mut exhausted_current_turn = false;
            let mut baseline_marks = None;
            if let Some(mut pending) = pending.and_then(|s| serde_json::from_str::<Value>(&s).ok())
                && let (Some(previous), Some(target)) = (pending["attempt"].as_str().map(str::to_owned), pending["target_tokens"].as_u64())
                && previous != attempt
            {
                let same_model: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM model_attempt old JOIN model_attempt new ON old.profile_id=new.profile_id AND old.model_id=new.model_id WHERE old.id=?1 AND new.id=?2)", params![previous,attempt], |row| row.get(0))?;
                if !same_model {
                    db.execute("DELETE FROM app_meta WHERE key=?1", [&target_key])?;
                } else {
                    if pending["verified"].is_null() {
                        let insufficient = target::verify(target, input_tokens) == target::Verification::Insufficient;
                        let outcome = if insufficient {"insufficient"} else {"met"};
                        let verification = json!({"attempt_id":attempt,"input_tokens":input_tokens,"target_tokens":target,"outcome":outcome}).to_string();
                        db.execute("UPDATE model_attempt SET error_json=json_set(error_json,'$.automatic_trim_attempt.target_verification',json(?1)) WHERE id=?2 AND json_valid(error_json)", params![verification,previous])?;
                        pending["verified"] = json!(outcome);
                        db.execute("UPDATE app_meta SET value=?2 WHERE key=?1", params![target_key,pending.to_string()])?;
                    }
                    if pending["verified"] == "met" {
                        db.execute("DELETE FROM app_meta WHERE key=?1", [&target_key])?;
                    } else if pending["scope"] == "prefix" {
                        insufficient_prefix = true;
                    } else if pending["scope"] == "spill" {
                        insufficient_spill = true;
                    } else if pending["scope"] == "active_turn" && pending["turn"] == current_turn {
                        exhausted_current_turn = true;
                        baseline_marks = serde_json::from_value(pending["marks"].clone()).ok();
                    }
                }
            }
            if input_tokens < threshold && !insufficient_prefix && !insufficient_spill && !exhausted_current_turn {
                return Ok(Some(Measurement::BelowThreshold));
            }
            let input_characters = db.query_row(
                "SELECT CASE WHEN json_valid(input_manifest_json) AND json_type(input_manifest_json,'$.input_characters')='integer' THEN json_extract(input_manifest_json,'$.input_characters') END FROM model_attempt WHERE id=?1",
                [attempt], |row| row.get::<_, Option<i64>>(0),
            )?.and_then(|n| u64::try_from(n).ok());
            Ok(Some(Measurement::Ready(Evidence { attempt: attempt.to_owned(), input_tokens, input_characters, insufficient_prefix, exhausted_active: exhausted_current_turn, baseline_marks })))
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
    // #182: while the tool loop is open, spill opportunities are deferred,
    // not seen: keep them out of the progress guard so the closed-loop
    // boundary still finds them new.
    let open_loop = control.compactor.tool_loop_open(&snapshot.thread).await?;
    let marks = candidates.as_ref().map(|candidate| {
        candidate
            .marks
            .iter()
            .filter(|mark| !(open_loop && mark.starts_with("spill:")))
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
    let widened_active = evidence.exhausted_active && match (&evidence.baseline_marks, &marks) {
        (Some(previous), Some(current)) => guard::has_new_marks(previous, current, "middle:"),
        _ => false,
    };
    let spill_new = !evidence.insufficient_prefix && !evidence.exhausted_active && has_kind("spill:");
    let summary_new = !evidence.insufficient_prefix
        && has_kind("summary:")
        && !(marks.is_none()
            && budget
                .guard
                .as_ref()
                .is_some_and(|guard| guard.blocks_unknown_summary(snapshot)));
    let middle_new = if evidence.exhausted_active { widened_active } else { has_kind("middle:") };
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
    let prefer_active = evidence.insufficient_prefix || std::mem::take(&mut budget.prefix_adopted);
    let mut summary_scope = "";
    let result: Result<Value, ApiError> = async {
    let before_characters = if target_plan.is_some() {
        Some(control.compactor.proactive_history_characters(&snapshot.thread).await?)
    } else { None };
    // #182: spill rewrites an earlier tool result while later output is
    // kept, so it waits for a boundary where the tool loop is closed.
    let spill = if spill_new && !open_loop {
        control.compactor.spill_reducing_cancellable(&snapshot.thread, slot.stop.clone()).await
    } else {
        Ok(crate::compaction::CompactResult { no_target: true, ..Default::default() })
    };
    let observation = match spill {
        Err(error) if error.is_storage_failure() => return Err(error),
        Err(error) => json!({"outcome":"rejected","error":error}),
        Ok(spill) if !any_summary_new => json!({
            "outcome": if spill.no_target { "no_safe_reduction" } else { "adopted" },
            "summary": spill,
            "summary_deferred": true,
        }),
        Ok(spill) => match summarize_once(control, &snapshot.thread, slot, Order { prefer_active, middle_new, ordinary_new: summary_new }, &mut midturn, &mut summary_scope).await
        {
            Ok(mut summary) => {
                if summary_scope == "prefix" && middle_new && !slot.stop.is_cancelled()
                    && let (Some(plan), Some(before)) = (target_plan, before_characters)
                {
                    let after = control.compactor.proactive_history_characters(&snapshot.thread).await?;
                    if before.saturating_sub(after) < plan.remove_characters {
                        match control.compactor.summarize_active_turn(&snapshot.thread, slot.stop.clone()).await {
                            Ok(active) if !active.no_target => {
                                summary.spilled_groups += active.spilled_groups;
                                summary.source_nodes += active.source_nodes;
                                summary.original_node_ids.extend(active.original_node_ids);
                                summary.no_target = false;
                                midturn = json!({"scope":"active_turn", "reason":"target_shortfall"});
                                summary_scope = "active_turn";
                            }
                            Ok(_) => {}
                            Err(error) if error.is_storage_failure() || slot.stop.is_cancelled() => return Err(error),
                            Err(error) => {
                                summary.spilled_groups += spill.spilled_groups;
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
    let mut observation = result?;
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
                    after.as_ref().map(|candidate| {
                        candidate
                            .marks
                            .iter()
                            .filter(|mark| !(open_loop && mark.starts_with("spill:")))
                            .cloned()
                            .collect()
                    }),
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
        guard::candidates(control, &snapshot.thread, None).await.ok().flatten().map(|c| c.marks)
    } else { None };
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
    Ok(true)
}

struct Order {
    prefer_active: bool,
    middle_new: bool,
    ordinary_new: bool,
}

/// One summary operation per attempt. #182 order: the ordinary summary of
/// earlier Turns first (oldest context matters least); the active-Turn
/// summary only when that prefix has nothing but earlier summaries, or when
/// the previous attempt adopted the prefix summary and a fresh measurement is
/// still at or above the threshold (`prefer_active`).
async fn summarize_once(
    control: &Arc<Control>,
    thread: &str,
    slot: &DriverSlot,
    order: Order,
    midturn: &mut Value,
    scope: &mut &'static str,
) -> Result<crate::compaction::CompactResult, ApiError> {
    let compactor = &control.compactor;
    if !order.prefer_active && order.ordinary_new {
        let prefix = compactor
            .summarize_unsummarized_prefix(thread, slot.stop.clone())
            .await;
        if !matches!(&prefix, Ok(result) if result.no_target) {
            *midturn = json!({"skipped":"prefix_first"});
            *scope = "prefix";
            return prefix;
        }
    }
    if order.middle_new {
        let active = compactor
            .summarize_active_turn(thread, slot.stop.clone())
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
        return compactor
            .summarize_unsummarized_prefix(thread, slot.stop.clone())
            .await;
    }
    Ok(crate::compaction::CompactResult {
        no_target: true,
        ..Default::default()
    })
}
