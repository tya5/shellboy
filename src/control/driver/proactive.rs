//! Proactive threshold compaction and measured retry policy (#125, #153).
//! Uses ordinary safe boundaries, recording partial spill adoption separately.
use super::*;

#[cfg(test)]
mod fault_tests;

#[cfg(test)]
mod spill_route_tests {
    use super::{SpillRoute, spill_route};

    #[test]
    fn off_skips_a_routing_and_all_automatic_spill_stages() {
        for a in [None, Some(0), Some(50), Some(100)] {
            assert_eq!(spill_route(false, a, 50), SpillRoute::SummaryWithoutSpill);
        }
    }

    #[test]
    fn on_preserves_existing_a_threshold_split() {
        assert_eq!(spill_route(true, Some(50), 50), SpillRoute::SpillOnly);
        assert_eq!(
            spill_route(true, Some(51), 50),
            SpillRoute::SpillThenSummary
        );
    }
}

pub(super) const REASON: &str = "proactive_threshold";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpillRoute {
    SpillOnly,
    SpillThenSummary,
    SummaryWithoutSpill,
}

fn spill_route(effective: bool, a_input_tokens: Option<u64>, target: u64) -> SpillRoute {
    if !effective {
        SpillRoute::SummaryWithoutSpill
    } else if a_input_tokens.is_some_and(|a| a <= target) {
        SpillRoute::SpillOnly
    } else {
        SpillRoute::SpillThenSummary
    }
}

/// Kept as a caller handle; no durable progress/marks policy lives here.
/// One automatic attempt's identity for the #226 recoverable record.
struct Automatic {
    attempt: String,
    token: String,
    thread: String,
    turn: String,
}

impl Automatic {
    async fn observed<T>(
        &self,
        control: &Arc<Control>,
        phase: &str,
        operation: impl std::future::Future<Output = Result<T, ApiError>>,
    ) -> Result<T, ApiError> {
        use crate::compaction::discard::{CompactionOutcomeFact, CompactionPhase};
        let started = std::time::Instant::now();
        let (attempt, token, phase_key) =
            (self.attempt.clone(), self.token.clone(), phase.to_owned());
        control
            .store
            .database
            .call(move |db| {
                db.execute(
                    crate::compaction::discard::STAGE_SQL,
                    params![attempt, token, phase_key],
                )?;
                Ok(())
            })
            .await
            .map_err(storage_error)?;
        let result = operation.await;
        if let Err(error) = &result {
            let fact = CompactionOutcomeFact {
                outcome: crate::compaction::discard::cause(error),
                phase: CompactionPhase::from_stage(phase).expect("closed stage name"),
                elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            };
            let body = serde_json::to_string(&fact).expect("bounded fact");
            let (attempt, token, phase_key) =
                (self.attempt.clone(), self.token.clone(), phase.to_owned());
            let saved = control
                .store
                .database
                .call(move |db| {
                    Ok(db.execute(
                        crate::compaction::discard::SAVE_SQL,
                        params![attempt, token, body, phase_key],
                    )? == 1)
                })
                .await;
            // A second recording failure must not mask the original fatal store error.
            let saved = match saved {
                Ok(saved) => saved,
                Err(_) if error.is_storage_failure() => return result,
                Err(error) => return Err(storage_error(error)),
            };
            if saved {
                control.diagnostics.record_compaction_outcome(
                    crate::diagnostics::DiagnosticIds {
                        attempt_id: Some(self.attempt.clone()),
                        thread_id: Some(self.thread.clone()),
                        turn_id: Some(self.turn.clone()),
                        ..Default::default()
                    },
                    fact,
                );
            }
        }
        result
    }

    fn stage(&self, stage: &'static str) -> crate::compaction::AutomaticStage {
        crate::compaction::AutomaticStage {
            attempt_id: self.attempt.clone(),
            token: self.token.clone(),
            stage,
        }
    }
}

// Shared real stage boundary, including fault-only postcommit observation.
async fn summary_stage(
    control: &Arc<Control>,
    compactor: &crate::compaction::Compactor,
    thread: &str,
    slot: &DriverSlot,
    active: bool,
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
    automatic: &Automatic,
) -> Result<crate::compaction::CompactResult, ApiError> {
    let stage = if active {
        "active_summary"
    } else {
        "closed_summary"
    };
    let result = if active {
        automatic
            .observed(
                control,
                stage,
                compactor.summarize_active_guarded(
                    thread,
                    slot.stop.clone(),
                    Some(boundary),
                    Some(automatic.stage(stage)),
                ),
            )
            .await?
    } else {
        automatic
            .observed(
                control,
                stage,
                compactor.summarize_prefix_guarded(
                    thread,
                    slot.stop.clone(),
                    Some(boundary),
                    Some(automatic.stage(stage)),
                ),
            )
            .await?
    };
    #[cfg(test)]
    if !result.no_target {
        fault_tests::checkpoint(control, slot, stage).await?;
    }
    Ok(result)
}

pub(super) async fn maybe_compact(
    control: &Arc<Control>,
    snapshot: &mut Snapshot,
    slot: &DriverSlot,
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
) -> Result<bool, ApiError> {
    // One decision per fresh provider report; no character forecast or marks.
    let thread = snapshot.thread.clone();
    let config = control.store.config.clone();
    let data = control
        .store
        .database
        .call(move |db| {
            let settings: (Option<i64>, Option<i64>, String, Option<bool>, i64) = db.query_row(
                "SELECT auto_compact_at,auto_compact_target_percent,profile_id,spill_override,revision FROM thread WHERE id=?1",
                [&thread],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, crate::storage::decode_spill_override(r.get_ref(3)?, 3)?, r.get(4)?)),
            )?;
            let (usage, _) = crate::compaction::context_usage::prepare_context_usage(
                crate::compaction::context_usage::read_context_usage_tx(db, &thread, None)?,
            );
            let current = config.current();
            let connection_spill = current.connections.get(&settings.2).and_then(|c| c.spill);
            let spill = crate::config::SpillPolicy::resolve(settings.3, connection_spill);
            let a = if spill.effective {
                let state = crate::compaction::a_state::read(db, &thread)?;
                Some(match crate::compaction::a_state::reading(state.as_ref()) {
                    crate::compaction::a_state::Reading::LegacyAbsent
                    | crate::compaction::a_state::Reading::AwaitingFirst => 0,
                    crate::compaction::a_state::Reading::Known(n) => n,
                })
            } else {
                None
            };
            Ok((settings, usage, spill, a))
        })
        .await
        .map_err(storage_error)?;
    let connection = snapshot.clients.connections.get(&snapshot.profile);
    let threshold = control.store.config.compaction.auto_at_tokens_for(
        data.0
            .0
            .map(|n| {
                u64::try_from(n).map_err(|_| {
                    ApiError::new(ErrorCode::InvalidState, "Invalid automatic threshold")
                })
            })
            .transpose()?,
        connection,
    );
    let percent = crate::config::SummarySettings::resolve(
        data.0
            .1
            .map(|n| {
                u64::try_from(n).map_err(|_| {
                    ApiError::new(ErrorCode::InvalidState, "Invalid automatic percentage")
                })
            })
            .transpose()?,
        None,
        connection,
        None,
        &control.store.config.compaction,
    )
    .target_percent;
    if threshold == 0 {
        return Ok(false);
    }
    let report = &data.1["last_successful_attempt"];
    let (Some(input), Some(attempt)) = (
        report["input_tokens"].as_u64(),
        report["attempt_id"].as_str(),
    ) else {
        return Ok(false);
    };
    if input < threshold || report["next_request_pending"] == true {
        return Ok(false);
    }
    let a = data.3;
    let target = ((threshold as u128 * percent as u128) / 100) as u64;
    let compactor = control.compactor.pinned(snapshot.clients.clone());
    let automatic = Automatic {
        attempt: attempt.into(),
        token: uuid::Uuid::new_v4().to_string(),
        thread: snapshot.thread.clone(),
        turn: snapshot.turn.clone(),
    };
    let (id, token, thread) = (
        automatic.attempt.clone(),
        automatic.token.clone(),
        snapshot.thread.clone(),
    );
    let trigger = json!({"auto_at_tokens":threshold,"target_percent":percent,"target_tokens":target,"input_tokens":input,"a_input_tokens":a,"run_id":snapshot.run});
    // The TUI activity indicator (#165) reads this witness while the attempt runs.
    let active_key = crate::compaction::CompactionKeys::new(&snapshot.thread).automatic_active();
    let witness = json!({"run_id": snapshot.run, "turn_id": snapshot.turn,
        "input_tokens": input, "threshold": threshold,
        "started_at": chrono::Utc::now().to_rfc3339()})
    .to_string();
    let (saved_key, saved_witness) = (active_key.clone(), witness.clone());
    let ids = crate::diagnostics::DiagnosticIds {
        thread_id: Some(snapshot.thread.clone()),
        run_id: Some(snapshot.run.clone()),
        turn_id: Some(snapshot.turn.clone()),
        ..Default::default()
    };
    let sampled_profile = data.0.2.clone();
    let sampled_revision = data.0.4;
    let guard_thread = thread.clone();
    let can_run=control.store.database.call(move |db| {
        let tx=db.transaction()?;
        let current: Option<(String, i64)> = tx.query_row("SELECT profile_id,revision FROM thread WHERE id=?1",[&guard_thread],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if current != Some((sampled_profile, sampled_revision)) { return Ok(false); }
        let consumed:bool=tx.query_row("SELECT json_type(error_json,'$.automatic_trim_attempt') IS NOT NULL FROM model_attempt WHERE id=?1",[&id],|r|r.get(0))?;
        if consumed {return Ok(false);}
        crate::compaction::attempt_begin_tx(
            &tx,
            &id,
            &token,
            REASON,
            if data.2.effective {
                crate::compaction::ABoundaryObservation::Observe
            } else {
                crate::compaction::ABoundaryObservation::Unobserved
            },
            &trigger,
            &Value::Null,
        )?;
        tx.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![saved_key, saved_witness])?;
        // Retire old target/guard metadata, never the historical attempt record.
        for key in [crate::compaction::CompactionKeys::new(&thread).automatic_target(),crate::compaction::CompactionKeys::new(&thread).automatic_guard(),crate::compaction::CompactionKeys::new(&thread).automatic_status()] {tx.execute("DELETE FROM app_meta WHERE key=?1",[key])?;}
        tx.commit()?;Ok(true)
    }).await.map_err(storage_error)?;
    if !can_run {
        return Ok(false);
    }
    control.diagnostics.record(
        crate::diagnostics::DiagnosticEvent::AutomaticCompactionStarted,
        ids.clone(),
    );
    control.changed(&snapshot.thread);
    snapshot.input.take();
    // The spill committed before this attempt's summary stage, if any. The
    // terminal record aggregates it with the summary (compact list reads it).
    let mut committed_spill: Option<crate::compaction::CompactResult> = None;
    let result: Result<crate::compaction::CompactResult, ApiError> = async {
        let route = spill_route(data.2.effective, a, target);
        if route == SpillRoute::SpillOnly {
            let spilled = automatic
                .observed(
                    control,
                    "closed_spill",
                    compactor.spill_staged_cancellable(
                        &snapshot.thread,
                        false,
                        u64::MAX,
                        slot.stop.clone(),
                        Some(automatic.stage("closed_spill")),
                    ),
                )
                .await?;
            #[cfg(test)]
            if !spilled.no_target {
                // Keep the fault hook at every real postcommit stage boundary;
                // closed-spill-only attempts do not pass through summary_stage.
                fault_tests::checkpoint(control, slot, "closed_spill").await?;
            }
            return Ok(spilled);
        }
        // With spill disabled, keep the same summary/adoption path but skip
        // both automatic spill stages and the #A range-selection read.
        let active = !compactor.has_closed_summary(&snapshot.thread).await?;
        if route == SpillRoute::SpillThenSummary {
            let spill_phase = if active {
                "active_spill"
            } else {
                "closed_spill"
            };
            let spilled = automatic
                .observed(
                    control,
                    spill_phase,
                    compactor.spill_staged_cancellable(
                        &snapshot.thread,
                        active,
                        u64::MAX,
                        slot.stop.clone(),
                        Some(automatic.stage(spill_phase)),
                    ),
                )
                .await?;
            #[cfg(test)]
            if !spilled.no_target {
                fault_tests::checkpoint(control, slot, spill_phase).await?;
            }
            if !spilled.no_target {
                committed_spill = Some(spilled);
            }
        }
        summary_stage(
            control,
            &compactor,
            &snapshot.thread,
            slot,
            active,
            boundary,
            &automatic,
        )
        .await
    }
    .await;
    // A retiring owner must not remove a newer owner's witness.
    let cleanup = control
        .store
        .database
        .call(move |db| {
            db.execute(
                "DELETE FROM app_meta WHERE key=?1 AND value=?2",
                params![active_key, witness],
            )?;
            Ok(())
        })
        .await
        .map_err(storage_error);
    control.diagnostics.record(
        crate::diagnostics::DiagnosticEvent::AutomaticCompactionFinished,
        ids,
    );
    control.changed(&snapshot.thread);
    cleanup?;
    let result = match result {
        Ok(compacted) => compactor
            .proactive_history_characters(&snapshot.thread)
            .await
            .map(|_| compacted),
        Err(error) => Err(error),
    };
    // Aggregate of every stage this attempt adopted: the committed spill plus
    // the summary. A spill committed before a failed summary stays adopted.
    let summary = match (&result, committed_spill) {
        (Ok(result), spill) => {
            let mut summary = result.clone();
            if let Some(spill) = spill {
                summary.spilled_groups += spill.spilled_groups;
                summary.source_nodes += spill.source_nodes;
                summary.original_node_ids.extend(spill.original_node_ids);
                summary.no_target = summary.no_target && spill.no_target;
            }
            Some(json!(summary))
        }
        (Err(error), Some(spill)) => {
            let mut summary = json!(spill);
            summary["summary_rejected"] = json!(true);
            if let Some(details) = error.details.as_ref().and_then(Value::as_object) {
                for field in [
                    "summary_usage",
                    "source_characters",
                    "summary_characters",
                    "summary_limit",
                    "model_requests",
                ] {
                    if let Some(value) = details.get(field) {
                        summary[field] = value.clone();
                    }
                }
            }
            Some(summary)
        }
        (Err(_), None) => None,
    };
    let no_target = summary
        .as_ref()
        .is_some_and(|summary| summary["no_target"] == true);
    let mut record = json!({"outcome":match &result {Ok(_) if no_target=>"no_target",Ok(_)=>"adopted",Err(e) if slot.stop.is_cancelled() && !e.is_storage_failure()=>"cancelled",Err(_)=>"failed"},"provisional":false,"reason":REASON,"trigger":{"auto_at_tokens":threshold,"target_percent":percent,"target_tokens":target,"input_tokens":input,"a_input_tokens":a},"target_plan":null});
    if let Some(summary) = summary {
        record["summary"] = summary;
    }
    if let Err(error) = &result {
        record["error"] = serde_json::to_value(error).unwrap_or(Value::Null);
    }
    let (attempt, token) = (automatic.attempt.clone(), automatic.token.clone());
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            crate::compaction::attempt_finish_tx(&tx, &record.to_string(), &attempt, &token)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(storage_error)?;
    control.changed(&snapshot.thread);
    // The truthful terminal record is saved first. A storage failure is then
    // still a driver storage failure (fail_storage, owner shutdown; Control
    // "アプリ保存障害"), never a recorded failure followed by the next request.
    if let Err(error) = result
        && (slot.stop.is_cancelled() || error.is_storage_failure())
    {
        return Err(error);
    }
    Ok(true)
}
