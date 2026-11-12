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
    /// #248: summary stages rejected earlier in this Turn.
    blocks: Blocks,
}

/// A summary stage that failed (not cancelled) is not tried again in the same
/// Turn while the forecast is unknown, until that stage adopts a summary. With
/// known marks, closed rejection survives unrelated spill/active adoption until
/// a new closed-summary source appears. The
/// Run-local state is simply lost on restart (one more attempt is allowed).
#[derive(Default)]
struct Blocks {
    turn: String,
    closed: bool,
    active: bool,
    closed_marks: Option<Vec<String>>,
}

impl Blocks {
    fn note(
        &mut self,
        turn: &str,
        closed: Option<&str>,
        active: Option<&str>,
        marks: Option<&[String]>,
    ) {
        if self.turn != turn {
            *self = Self {
                turn: turn.to_owned(),
                ..Self::default()
            };
        }
        if closed == Some("rejected") {
            self.closed_marks = marks.map(<[String]>::to_vec);
        }
        if closed == Some("adopted") {
            self.closed_marks = None;
        }
        for (flag, outcome) in [(&mut self.closed, closed), (&mut self.active, active)] {
            match outcome {
                Some("rejected") => *flag = true,
                Some("adopted") => *flag = false,
                _ => {}
            }
        }
    }
    fn closed_for(&self, turn: &str, marks: Option<&[String]>) -> bool {
        self.turn == turn
            && self.closed
            && match (&self.closed_marks, marks) {
                (Some(previous), Some(current)) => {
                    !super::proactive_guard::has_new_marks(previous, current, "summary:")
                }
                // Unknown coverage keeps the #248 per-stage Turn block.
                (_, None) => true,
                (None, Some(_)) => false,
            }
    }
    fn active_for(&self, turn: &str) -> bool {
        self.turn == turn && self.active
    }
}

/// One automatic attempt's identity for the #226 recoverable record.
struct Automatic {
    attempt: String,
    token: String,
    thread: String,
    turn: String,
    target_tokens: Option<u64>,
}

impl Automatic {
    async fn observed<T>(
        &self,
        control: &Arc<Control>,
        phase: &str,
        operation: impl std::future::Future<Output = Result<T, ApiError>>,
    ) -> Result<T, ApiError> {
        use crate::compaction::discard::{CompactionDiscard, DiscardPhase, DiscardReason};
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
        if let Err(error) = &result
            && error.code == ErrorCode::InvalidState
        {
            let reason = error
                .details
                .as_ref()
                .and_then(|v| v.get("compaction_discard_reason"))
                .and_then(|v| serde_json::from_value::<DiscardReason>(v.clone()).ok())
                .unwrap_or(DiscardReason::Other);
            let fact = CompactionDiscard {
                reason,
                phase: DiscardPhase::from_stage(phase).expect("closed stage name"),
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
                .await
                .map_err(storage_error)?;
            if saved {
                control.diagnostics.record_compaction_discard(
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

    /// The calibrated provisional target uses the scope W4 would give the
    /// stage; `marks` stays null, the documented coverage-limited baseline.
    fn stage(&self, stage: &'static str) -> crate::compaction::AutomaticStage {
        let scope = match stage {
            "closed_spill" => "spill",
            "closed_summary" => "prefix",
            _ => "active_turn",
        };
        crate::compaction::AutomaticStage {
            attempt_id: self.attempt.clone(),
            token: self.token.clone(),
            stage,
            target: self.target_tokens.map(|tokens| {
                (
                    crate::compaction::CompactionKeys::new(&self.thread).automatic_target(),
                    json!({"attempt":self.attempt,"target_tokens":tokens,"scope":scope,"turn":self.turn,"marks":null,"provisional":true}).to_string(),
                )
            }),
        }
    }
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
    // One decision per fresh provider report; no character forecast or marks.
    let thread=snapshot.thread.clone();
    let data=control.store.database.call(move |db| {
        let settings:(Option<u64>,Option<u64>)=db.query_row("SELECT auto_compact_at,auto_compact_target_percent FROM thread WHERE id=?1",[&thread],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let (usage,_)=crate::compaction::context_usage::prepare_context_usage(crate::compaction::context_usage::read_context_usage_tx(db,&thread,None)?);
        let state=crate::compaction::a_state::read(db,&thread)?;
        Ok((settings,usage,crate::compaction::a_state::reading(state.as_ref())))
    }).await.map_err(storage_error)?;
    let connection=snapshot.clients.connections.get(&snapshot.profile);
    let threshold=control.store.config.compaction.auto_at_tokens_for(data.0.0,connection);
    let percent=crate::config::target_percent_for(data.0.1,connection);
    if threshold==0 {return Ok(false);}
    let report=&data.1["last_successful_attempt"];
    let (Some(input),Some(attempt))=(report["input_tokens"].as_u64(),report["attempt_id"].as_str()) else {return Ok(false);};
    if input<threshold || report["next_request_pending"]==true {return Ok(false);}
    use crate::compaction::a_state::Reading;
    let a=match data.2 {Reading::LegacyAbsent|Reading::AwaitingFirst=>0,Reading::Known(n)=>n,Reading::Unknown=>return Ok(false)};
    let target=((threshold as u128*percent as u128)/100) as u64;
    let compactor=control.compactor.pinned(snapshot.clients.clone());
    let automatic=Automatic {attempt:attempt.into(),token:uuid::Uuid::new_v4().to_string(),thread:snapshot.thread.clone(),turn:snapshot.turn.clone(),target_tokens:None};
    let (id,token,thread)=(automatic.attempt.clone(),automatic.token.clone(),snapshot.thread.clone());
    let trigger=json!({"auto_at_tokens":threshold,"target_percent":percent,"target_tokens":target,"input_tokens":input,"a_input_tokens":a,"run_id":snapshot.run});
    let can_run=control.store.database.call(move |db| {
        let tx=db.transaction()?;
        let consumed:bool=tx.query_row("SELECT json_type(error_json,'$.automatic_trim_attempt') IS NOT NULL FROM model_attempt WHERE id=?1",[&id],|r|r.get(0))?;
        if consumed {return Ok(false);}
        crate::compaction::attempt_begin_tx(&tx,&id,&token,REASON,&trigger,&Value::Null)?;
        // Retire old target/guard metadata, never the historical attempt record.
        let keys=crate::compaction::CompactionKeys::new(&thread);
        for key in [keys.automatic_target(),keys.automatic_guard(),keys.automatic_status()] {tx.execute("DELETE FROM app_meta WHERE key=?1",[key])?;}
        tx.commit()?;Ok(true)
    }).await.map_err(storage_error)?;
    if !can_run {return Ok(false);}
    snapshot.input.take();
    let result:Result<crate::compaction::CompactResult,ApiError>=async {
        if a<=target {
            return automatic.observed(control,"closed_spill",compactor.spill_staged_cancellable(&snapshot.thread,false,u64::MAX,slot.stop.clone(),Some(automatic.stage("closed_spill")))).await;
        }
        // Choose the summary region once, then spill only inside that region.
        let active=!compactor.has_closed_summary(&snapshot.thread).await?;
        let spill_phase=if active {"active_spill"}else{"closed_spill"};
        automatic.observed(control,spill_phase,compactor.spill_staged_cancellable(&snapshot.thread,active,u64::MAX,slot.stop.clone(),Some(automatic.stage(spill_phase)))).await?;
        if active {
            automatic.observed(control,"active_summary",compactor.summarize_active_guarded(&snapshot.thread,slot.stop.clone(),Some(&mut *boundary),Some(automatic.stage("active_summary")))).await
        } else {
            automatic.observed(control,"closed_summary",compactor.summarize_prefix_guarded(&snapshot.thread,slot.stop.clone(),Some(&mut *boundary),Some(automatic.stage("closed_summary")))).await
        }
    }.await;
    let mut record=json!({"outcome":match &result {Ok(r) if r.no_target=>"no_target",Ok(_)=>"adopted",Err(_) if slot.stop.is_cancelled()=>"cancelled",Err(_)=>"failed"},"provisional":false,"reason":REASON,"trigger":{"auto_at_tokens":threshold,"target_percent":percent,"target_tokens":target,"input_tokens":input,"a_input_tokens":a},"target_plan":null});
    if let Err(error)=&result {record["error"]=serde_json::to_value(error).unwrap_or(Value::Null);}
    let (attempt,token)=(automatic.attempt.clone(),automatic.token.clone());
    control.store.database.call(move |db| {let tx=db.transaction()?;crate::compaction::attempt_finish_tx(&tx,&record.to_string(),&attempt,&token)?;tx.commit()?;Ok(())}).await.map_err(storage_error)?;
    control.changed(&snapshot.thread);
    let _=budget; // Retired progress fields are removed in the API cleanup child.
    if slot.stop.is_cancelled() {if let Err(error)=result {return Err(error);}}
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
    _control: &Arc<Control>,
    compactor: &crate::compaction::Compactor,
    thread: &str,
    slot: &DriverSlot,
    remaining: Option<u64>,
    allow_summary: bool,
    stages: &mut Value,
    committed: &mut Vec<crate::compaction::CompactResult>,
    boundary: &mut crate::compaction::boundary::Boundary<'_>,
    automatic: &Automatic,
) -> Result<crate::compaction::CompactResult, ApiError> {
    let Some(need) = remaining else {
        // #226: the uncalibrated active-Turn summary is a recorded stage too.
        let summary = automatic
            .observed(
                _control,
                "active_summary",
                compactor.summarize_active_guarded(
                    thread,
                    slot.stop.clone(),
                    Some(&mut *boundary),
                    Some(automatic.stage("active_summary")),
                ),
            )
            .await;
        if let Ok(summary) = &summary {
            committed.push(summary.clone());
        }
        stages["active_summary"] = json!({"ran":true,"outcome":match &summary {
            Ok(r) if r.no_target => "no_target",
            Ok(_) => "adopted",
            Err(_) if slot.stop.is_cancelled() => "cancelled",
            Err(_) => "rejected",
        }});
        #[cfg(test)]
        if summary.as_ref().is_ok_and(|r| !r.no_target) {
            fault_tests::checkpoint(_control, slot, "active_summary").await?;
        }
        return summary;
    };
    let before = compactor.proactive_history_characters(thread).await?;
    stages["active_spill"] = json!({"ran":true,"remaining_before":need,"outcome":"started"});
    let spill = match automatic
        .observed(
            _control,
            "active_spill",
            compactor.spill_staged_cancellable(
                thread,
                true,
                need,
                slot.stop.clone(),
                Some(automatic.stage("active_spill")),
            ),
        )
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
        fault_tests::checkpoint(_control, slot, "active_spill").await?;
    }
    stages["active_spill"]["adopted_groups"] = json!(spill.spilled_groups);
    stages["active_spill"]["outcome"] = json!(if spill.no_target {
        "no_target"
    } else {
        "adopted"
    });
    let after = compactor.proactive_history_characters(thread).await?;
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
    let (mut summary, projected_shortfall) = match automatic
        .observed(
            _control,
            "active_summary",
            compactor.summarize_target_active_guarded(
                thread,
                remaining,
                slot.stop.clone(),
                Some(&mut *boundary),
                Some(automatic.stage("active_summary")),
            ),
        )
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
        fault_tests::checkpoint(_control, slot, "active_summary").await?;
    }
    stages["active_summary"]["outcome"] = json!(if summary.no_target {
        "no_target"
    } else {
        "adopted"
    });
    let after_summary = compactor.proactive_history_characters(thread).await?;
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
    compactor: &crate::compaction::Compactor,
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
    automatic: &Automatic,
) -> Result<crate::compaction::CompactResult, ApiError> {
    if !order.prefer_active && order.ordinary_new {
        let before = if remaining.is_some() {
            Some(compactor.proactive_history_characters(thread).await?)
        } else {
            None
        };
        stages["closed_summary"] =
            json!({"ran":true,"remaining_before":remaining,"outcome":"started"});
        let prefix = if let Some(target) = remaining {
            automatic
                .observed(
                    control,
                    "closed_summary",
                    compactor.summarize_target_prefix_guarded(
                        thread,
                        target,
                        slot.stop.clone(),
                        Some(&mut *boundary),
                        Some(automatic.stage("closed_summary")),
                    ),
                )
                .await
                .map(|(result, missing)| {
                    *shortfall = Some(missing);
                    result
                })
        } else {
            automatic
                .observed(
                    control,
                    "closed_summary",
                    compactor.summarize_prefix_guarded(
                        thread,
                        slot.stop.clone(),
                        Some(&mut *boundary),
                        Some(automatic.stage("closed_summary")),
                    ),
                )
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
            compactor,
            thread,
            slot,
            remaining,
            order.middle_new,
            stages,
            committed,
            boundary,
            automatic,
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
        // #226: the late prefix summary is recorded and checkpointed like the
        // first-position one.
        let prefix = if let Some(target) = remaining {
            automatic
                .observed(
                    control,
                    "closed_summary",
                    compactor.summarize_target_prefix_guarded(
                        thread,
                        target,
                        slot.stop.clone(),
                        Some(&mut *boundary),
                        Some(automatic.stage("closed_summary")),
                    ),
                )
                .await
                .map(|(result, missing)| {
                    *shortfall = Some(missing);
                    result
                })
        } else {
            automatic
                .observed(
                    control,
                    "closed_summary",
                    compactor.summarize_prefix_guarded(
                        thread,
                        slot.stop.clone(),
                        Some(&mut *boundary),
                        Some(automatic.stage("closed_summary")),
                    ),
                )
                .await
        };
        if let Ok(summary) = &prefix {
            committed.push(summary.clone());
        }
        stages["closed_summary"] = json!({"ran":true,"remaining_before":remaining,"outcome":match &prefix {
            Ok(r) if r.no_target => "no_target",
            Ok(_) => "adopted",
            Err(_) if slot.stop.is_cancelled() => "cancelled",
            Err(_) => "rejected",
        }});
        #[cfg(test)]
        if prefix.as_ref().is_ok_and(|r| !r.no_target) {
            fault_tests::checkpoint(control, slot, "closed_summary").await?;
        }
        return prefix;
    }
    Ok(crate::compaction::CompactResult {
        no_target: true,
        ..Default::default()
    })
}

#[cfg(test)]
mod block_tests {
    use super::Blocks;

    #[test]
    fn only_failed_uncancelled_stages_block_until_they_adopt_or_the_turn_changes() {
        let mut blocks = Blocks::default();
        blocks.note("t1", Some("cancelled"), Some("no_target"), None);
        assert!(!blocks.closed_for("t1", None) && !blocks.active_for("t1"));
        blocks.note("t1", Some("rejected"), None, None);
        assert!(blocks.closed_for("t1", None) && !blocks.active_for("t1"));
        blocks.note("t1", None, Some("rejected"), None);
        assert!(blocks.closed_for("t1", None) && blocks.active_for("t1"));
        assert!(
            !blocks.closed_for("t2", None),
            "another Turn is never blocked"
        );
        blocks.note("t1", Some("adopted"), None, None);
        assert!(!blocks.closed_for("t1", None) && blocks.active_for("t1"));
        blocks.note("t2", None, None, None);
        assert!(!blocks.active_for("t1") && !blocks.active_for("t2"));
    }
}

#[cfg(test)]
mod known_block_tests {
    use super::Blocks;
    #[test]
    fn known_closed_rejection_survives_spill_and_active_adoption_but_not_new_closed_sources() {
        let mark = |kind: &str, id: char, chars: u64| {
            format!("{kind}:{}:{chars:016x}", id.to_string().repeat(64))
        };
        let old = vec![mark("summary", 'a', 50)];
        let mut blocks = Blocks::default();
        blocks.note("t", Some("rejected"), None, Some(&old));
        for i in 1..20 {
            let mut current = vec![mark("middle", 'b', i), mark("spill", 'c', i)];
            current.extend(old.clone());
            blocks.note("t", None, Some("adopted"), Some(&current));
            assert!(blocks.closed_for("t", Some(&current)));
        }
        assert!(!blocks.closed_for("next", Some(&old)));
        let grown = vec![mark("summary", 'a', 51)];
        assert!(!blocks.closed_for("t", Some(&grown)));
        let new = vec![mark("summary", 'd', 50)];
        assert!(!blocks.closed_for("t", Some(&new)));
        blocks.note("t", Some("adopted"), None, Some(&old));
        assert!(!blocks.closed_for("t", Some(&old)));
        blocks.note("t", Some("cancelled"), None, Some(&old));
        assert!(!blocks.closed_for("t", Some(&old)));
    }
}
