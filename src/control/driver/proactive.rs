//! Proactive threshold compaction and measured retry policy (#125, #153).
//! Uses ordinary safe boundaries, recording partial spill adoption separately.
use super::*;
use futures_util::FutureExt;

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
pub(super) enum ProactiveOutcome {
    NotStarted,
    Finished { path_changed: bool },
}

pub(in crate::control) struct BackgroundJob {
    cancel: tokio_util::sync::CancellationToken,
    started: Option<tokio::sync::oneshot::Receiver<()>>,
    task: tokio::task::JoinHandle<Result<ProactiveOutcome, ApiError>>,
}
impl BackgroundJob {
    #[cfg(test)]
    pub(in crate::control) fn test_started(
        cancel: tokio_util::sync::CancellationToken,
        task: tokio::task::JoinHandle<Result<ProactiveOutcome, ApiError>>,
    ) -> Self {
        Self {
            cancel,
            started: None,
            task,
        }
    }

    pub(in crate::control) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(in crate::control) fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(super) async fn wait_started(&mut self) -> Result<(), ApiError> {
        self.started
            .take()
            .expect("background launch acknowledgement consumed once")
            .await
            .map_err(|_| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "background compaction task panicked before launch",
                )
            })
    }

    pub(in crate::control) async fn join(
        &mut self,
    ) -> Result<Result<ProactiveOutcome, ApiError>, tokio::task::JoinError> {
        (&mut self.task).await
    }
}

fn terminal_attempt_outcome(
    result: &Result<crate::compaction::CompactResult, ApiError>,
    no_target: bool,
) -> &'static str {
    match result {
        Ok(_) if no_target => "no_target",
        Ok(_) => "adopted",
        Err(error) => crate::compaction::discard::terminal_attempt_status(
            crate::compaction::discard::cause(error),
        ),
    }
}

#[cfg(test)]
mod terminal_projection_tests {
    use super::terminal_attempt_outcome;
    use crate::compaction::discard::{
        self, CompactionOutcomeFact, CompactionPhase, Outcome, RejectionReason,
    };
    use crate::protocol::{ApiError, ErrorCode};

    fn rejected(reason: RejectionReason, message: &str) -> ApiError {
        let mut error = ApiError::new(ErrorCode::InvalidState, message);
        error.compaction_cause = Some(Box::new(Outcome::Rejected { reason }));
        error
    }

    #[test]
    fn captured_run_stopped_stays_failed_if_owner_token_cancels_before_terminal_projection() {
        let error = rejected(RejectionReason::RunStopped, "DB witness first");
        let captured = discard::cause(&error);
        let fact = CompactionOutcomeFact {
            phase: CompactionPhase::ClosedSummary,
            elapsed_ms: 1,
            outcome: captured,
        };
        let owner_stop = tokio_util::sync::CancellationToken::new();
        owner_stop.cancel();

        assert!(owner_stop.is_cancelled());
        assert_eq!(terminal_attempt_outcome(&Err(error), false), "failed");
        assert_eq!(
            fact.outcome,
            Outcome::Rejected {
                reason: RejectionReason::RunStopped
            }
        );
    }

    #[test]
    fn captured_cancelled_cause_projects_cancelled() {
        let error = rejected(RejectionReason::Cancelled, "owner token first");
        assert_eq!(terminal_attempt_outcome(&Err(error), false), "cancelled");
    }

    #[test]
    fn profile_and_storage_causes_project_failed() {
        let profile = rejected(RejectionReason::ProfileChanged, "profile changed");
        assert_eq!(terminal_attempt_outcome(&Err(profile), false), "failed");
        let storage = ApiError::new(ErrorCode::SaveFailed, "storage failed");
        assert_eq!(terminal_attempt_outcome(&Err(storage), false), "failed");
    }
}

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

// The owner scope carries the existing Run, boundary, and attempt lifetimes together.
struct SummaryStageOwner<'owner, 'boundary> {
    control: &'owner Arc<Control>,
    compactor: &'owner crate::compaction::Compactor,
    thread: &'owner str,
    boundary: &'owner mut crate::compaction::boundary::Boundary<'boundary>,
    automatic: &'owner Automatic,
    run_stop: tokio_util::sync::CancellationToken,
}

enum SummaryStageMode {
    Closed,
    Active { spill_after_no_target: bool },
}

#[derive(Debug)]
struct SummaryStageOutcome {
    summary: Result<crate::compaction::CompactResult, ApiError>,
    committed_active_spill: Option<crate::compaction::CompactResult>,
}

// Preserve the existing closed-summary-first route; active spill is a consequence
// of a true closed NoTarget, and its committed fact returns with the stage result.
async fn summary_stage(
    owner: SummaryStageOwner<'_, '_>,
    mode: SummaryStageMode,
    _slot: &DriverSlot,
) -> SummaryStageOutcome {
    let SummaryStageOwner {
        control,
        compactor,
        thread,
        boundary,
        automatic,
        run_stop,
    } = owner;
    let thread_boundary = control.boundary(thread).await;
    let admission_wait = tokio_util::sync::CancellationToken::new();
    let closed_stage = "closed_summary";
    let closed = match boundary
        .pause(
            &admission_wait,
            automatic.observed(
                control,
                closed_stage,
                compactor.run_gc_summary(
                    automatic.stage(closed_stage),
                    false,
                    automatic.turn.clone(),
                    thread_boundary.clone(),
                    run_stop.clone(),
                ),
            ),
        )
        .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            return SummaryStageOutcome {
                summary: Err(error),
                committed_active_spill: None,
            };
        }
        Err(_) => {
            return SummaryStageOutcome {
                summary: Err(invalid("GC boundary reacquisition cancelled")),
                committed_active_spill: None,
            };
        }
    };
    let mut committed_active_spill = None;
    let (_stage, summary) = match mode {
        SummaryStageMode::Active {
            spill_after_no_target,
        } if closed.no_target => {
            if spill_after_no_target {
                let active_spill = "active_spill";
                let spilled = match automatic
                    .observed(
                        control,
                        active_spill,
                        compactor.spill_staged_cancellable(
                            thread,
                            true,
                            u64::MAX,
                            run_stop.clone(),
                            Some(automatic.stage(active_spill)),
                        ),
                    )
                    .await
                {
                    Ok(spilled) => spilled,
                    Err(error) => {
                        return SummaryStageOutcome {
                            summary: Err(error),
                            committed_active_spill: None,
                        };
                    }
                };
                if !spilled.no_target {
                    committed_active_spill = Some(spilled);
                }
                #[cfg(test)]
                if committed_active_spill.is_some()
                    && let Err(error) = fault_tests::checkpoint(control, _slot, active_spill).await
                {
                    return SummaryStageOutcome {
                        summary: Err(error),
                        committed_active_spill,
                    };
                }
            }
            let active_stage = "active_summary";
            let result = match boundary
                .pause(
                    &admission_wait,
                    automatic.observed(
                        control,
                        active_stage,
                        compactor.run_gc_summary(
                            automatic.stage(active_stage),
                            true,
                            automatic.turn.clone(),
                            thread_boundary,
                            run_stop,
                        ),
                    ),
                )
                .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    return SummaryStageOutcome {
                        summary: Err(error),
                        committed_active_spill,
                    };
                }
                Err(_) => {
                    return SummaryStageOutcome {
                        summary: Err(invalid("GC boundary reacquisition cancelled")),
                        committed_active_spill,
                    };
                }
            };
            (active_stage, result)
        }
        _ => (closed_stage, closed),
    };
    #[cfg(test)]
    if !summary.no_target
        && let Err(error) = fault_tests::checkpoint(control, _slot, _stage).await
    {
        return SummaryStageOutcome {
            summary: Err(error),
            committed_active_spill,
        };
    }
    SummaryStageOutcome {
        summary: Ok(summary),
        committed_active_spill,
    }
}

pub(super) async fn maybe_compact_with_run_stop(
    control: &Arc<Control>,
    snapshot: &mut Snapshot,
    slot: &DriverSlot,
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
    run_stop: tokio_util::sync::CancellationToken,
) -> Result<ProactiveOutcome, ApiError> {
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
        return Ok(ProactiveOutcome::NotStarted);
    }
    let report = &data.1["last_successful_attempt"];
    let (Some(input), Some(attempt)) = (
        report["input_tokens"].as_u64(),
        report["attempt_id"].as_str(),
    ) else {
        return Ok(ProactiveOutcome::NotStarted);
    };
    if input < threshold || report["next_request_pending"] == true {
        return Ok(ProactiveOutcome::NotStarted);
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
        if current != Some((sampled_profile, sampled_revision)) { return Ok(None); }
        let consumed:bool=tx.query_row("SELECT json_type(error_json,'$.automatic_trim_attempt') IS NOT NULL FROM model_attempt WHERE id=?1",[&id],|r|r.get(0))?;
        if consumed {return Ok(None);}
        let captured_trigger = crate::compaction::attempt_begin_tx(
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
        tx.commit()?;Ok(Some(captured_trigger))
    }).await.map_err(storage_error)?;
    let Some(captured_trigger) = can_run else {
        return Ok(ProactiveOutcome::NotStarted);
    };
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
        let active = slot
            .turn
            .lock()
            .await
            .as_ref()
            .is_some_and(|turn| turn.turn == snapshot.turn);
        if route == SpillRoute::SpillThenSummary {
            // Commit an eligible closed spill first. Do not spill active work
            // until the shared closed-summary planner confirms NoTarget.
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
                fault_tests::checkpoint(control, slot, "closed_spill").await?;
            }
            if !spilled.no_target {
                committed_spill = Some(spilled);
            }
        }
        let mode = if active {
            SummaryStageMode::Active {
                spill_after_no_target: route == SpillRoute::SpillThenSummary,
            }
        } else {
            SummaryStageMode::Closed
        };
        let outcome = summary_stage(
            SummaryStageOwner {
                control,
                compactor: &compactor,
                thread: &snapshot.thread,
                boundary,
                automatic: &automatic,
                run_stop: run_stop.clone(),
            },
            mode,
            slot,
        )
        .await;
        if outcome.committed_active_spill.is_some() {
            committed_spill = outcome.committed_active_spill;
        }
        outcome.summary
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
    let path_changed = committed_spill
        .as_ref()
        .is_some_and(|spill| !spill.no_target)
        || result.as_ref().is_ok_and(|summary| !summary.no_target);
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
        (Err(error), None) => {
            let mut summary = serde_json::Map::new();
            summary.insert("summary_rejected".into(), json!(true));
            if let Some(details) = error.details.as_ref().and_then(Value::as_object) {
                for field in [
                    "summary_usage",
                    "source_characters",
                    "summary_characters",
                    "summary_limit",
                    "model_requests",
                ] {
                    if let Some(value) = details.get(field) {
                        summary.insert(field.into(), value.clone());
                    }
                }
            }
            (summary.len() > 1).then_some(Value::Object(summary))
        }
    };
    let no_target = summary
        .as_ref()
        .is_some_and(|summary| summary["no_target"] == true);
    let mut record = json!({"outcome":terminal_attempt_outcome(&result,no_target),"provisional":false,"reason":REASON,"trigger":captured_trigger,"target_plan":null});
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
    Ok(ProactiveOutcome::Finished { path_changed })
}
pub(super) fn spawn_background(
    control: Arc<Control>,
    mut snapshot: Snapshot,
    slot: DriverSlot,
    boundary: Arc<tokio::sync::Mutex<()>>,
    cancel: tokio_util::sync::CancellationToken,
) -> BackgroundJob {
    let task_cancel = cancel.clone();
    let (started_tx, started) = tokio::sync::oneshot::channel();
    let thread = snapshot.thread.clone();
    let control_for_cleanup = control.clone();
    let task = tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(async {
            let guard = tokio::select! {
                biased;
                _ = task_cancel.cancelled() => None,
                guard = boundary.lock() => Some(guard),
            };
            let _ = started_tx.send(());
            let Some(guard) = guard else {
                return Ok(ProactiveOutcome::NotStarted);
            };
            let mut boundary_owner = crate::compaction::boundary::Boundary::new(&boundary, guard);
            let result = maybe_compact_with_run_stop(
                &control,
                &mut snapshot,
                &slot,
                &mut boundary_owner,
                task_cancel,
            )
            .await;
            drop(boundary_owner.into_guard());
            result
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(invalid("background proactive compaction task panicked")));
        if matches!(&result, Err(error) if error.is_storage_failure()) {
            control.fail_storage().await;
        }
        // Keep the Thread slot until some Run or owner shutdown joins this handle.
        slot.nudge.notify_one();
        control_for_cleanup
            .live
            .prune_idle_background(&thread, &slot.proactive_active)
            .await;
        control_for_cleanup.changed(&thread);
        result
    });

    BackgroundJob {
        cancel,
        started: Some(started),
        task,
    }
}
