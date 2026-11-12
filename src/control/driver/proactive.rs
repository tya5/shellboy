//! Proactive threshold compaction and measured retry policy (#125, #153).
//! Uses ordinary safe boundaries, recording partial spill adoption separately.
use super::*;

#[cfg(test)]
mod fault_tests;

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
        let settings:(Option<i64>,Option<i64>)=db.query_row("SELECT auto_compact_at,auto_compact_target_percent FROM thread WHERE id=?1",[&thread],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let (usage,_)=crate::compaction::context_usage::prepare_context_usage(crate::compaction::context_usage::read_context_usage_tx(db,&thread,None)?);
        let state=crate::compaction::a_state::read(db,&thread)?;
        Ok((settings,usage,crate::compaction::a_state::reading(state.as_ref())))
    }).await.map_err(storage_error)?;
    let connection=snapshot.clients.connections.get(&snapshot.profile);
    let threshold=control.store.config.compaction.auto_at_tokens_for(data.0.0.map(|n|u64::try_from(n).map_err(|_|ApiError::new(ErrorCode::InvalidState,"Invalid automatic threshold"))).transpose()?,connection);
    let percent=crate::config::target_percent_for(data.0.1.map(|n|u64::try_from(n).map_err(|_|ApiError::new(ErrorCode::InvalidState,"Invalid automatic percentage"))).transpose()?,connection);
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
        for key in [crate::compaction::CompactionKeys::new(&thread).automatic_target(),crate::compaction::CompactionKeys::new(&thread).automatic_guard(),crate::compaction::CompactionKeys::new(&thread).automatic_status()] {tx.execute("DELETE FROM app_meta WHERE key=?1",[key])?;}
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
