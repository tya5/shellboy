//! Per-Thread model/tool loop. A long wait keeps its IPC slot for the whole wait.
mod adoption;
mod context_tail;
mod notification_admission;
mod overflow;
pub(super) mod proactive;
mod request;
mod retry;
pub(super) mod shell_output;
mod tools;
mod trim;
mod trim_observation;
use super::wait::wakes::Key;
use adoption::adopt_response;
use model::request_path::{
    AttemptKind, Execution, PreparedInput, RequestCaller, RequestScope, execute_request,
};
use overflow::OverflowBudget;
pub(crate) use request::{
    ContextLine, context_item, last_saved_context, leading_items, same_context,
};
use request::{Instructions, model_client, prepare_attempt, stream_observer};
use tools::run_tool_group;

use super::state::{now, recover_turn_tx, save_received_failure};
use super::*;
use crate::{
    history,
    model::{self, http::HttpModelClient},
    shell::ShellCompletion,
};
use futures_util::FutureExt;
use rusqlite::{OptionalExtension, params};

#[derive(Clone)]
struct Snapshot {
    clients: Arc<model::http::ClientSnapshot>,
    thread: String,
    run: String,
    turn: String,
    generation: i64,
    selection: i64,
    profile_revision: i64,
    profile: String,
    model: String,
    cache_comparison_response_id: Option<String>,
    cache_diagnostics_enabled: bool,
    /// Measured bytes of this attempt's request body (#186), set at preparation.
    input_bytes: Option<crate::model::wire_bytes::InputBytes>,
    effort: Option<String>,
    /// The Thread's own output limit; `None` keeps the connection value.
    max_tokens: Option<u64>,
    epoch: i64,
    cwd: PathBuf,
    agents: PathBuf,
    input: Option<super::replay::ReplayLease>,
    receipts: Vec<String>,
    events: Vec<String>,
}
enum Next {
    Ready(Box<Snapshot>),
    Idle,
    Recheck,
    Drain,
    Done,
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, message)
}

#[cfg(test)]
async fn admit_attempt(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    attempt: &str,
    attachments: Arc<crate::attachments::PreparedAttachments>,
    input: u64,
    history: u64,
) -> Result<bool, ApiError> {
    let digest = control
        .store
        .clients
        .get(&snapshot.profile)
        .ok()
        .and_then(|c| c.extra_digest());
    admit_attempt_with_digest(
        control,
        snapshot,
        attempt,
        attachments,
        input,
        history,
        digest,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn admit_attempt_with_digest(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    attempt: &str,
    attempt_attachments: Arc<crate::attachments::PreparedAttachments>,
    input_characters: u64,
    history_characters: u64,
    extra_digest: Option<String>,
) -> Result<bool, ApiError> {
    let current = snapshot.clone();
    let attempt_id = attempt.to_owned();
    let result = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let mut row = current.attempt_admission();
            let admitted = current.admit_attempt_tx(
                &tx,
                &mut row,
                &attempt_attachments,
                input_characters,
                history_characters,
                extra_digest,
            )?;
            if !admitted {
                return Ok(false);
            }
            model::request_path::admit_attempt(&tx, &attempt_id, row)?;
            current.link_attempt_tx(&tx, &attempt_id, &attempt_attachments)?;
            tx.commit()?;
            Ok(true)
        })
        .await
        .map_err(storage_error);
    if let Err(error) = &result
        && error.code != ErrorCode::OperationConflict
    {
        recover(control, snapshot, error.clone(), None).await?;
    }
    if matches!(result, Ok(true)) {
        // A hidden observer may already have consumed the preparing snapshot.
        control.changed(&snapshot.thread);
    }
    result
}

pub(super) async fn drive(
    control: Arc<Control>,
    thread: String,
    slot: DriverSlot,
) -> Result<(), ApiError> {
    let result = drive_inner(control.clone(), thread.clone(), slot.clone()).await;
    if result.is_err() || slot.stop.is_cancelled() {
        control.live.cancel_join_proactive_job(&thread, &slot).await;
    }
    result
}

async fn drive_inner(
    control: Arc<Control>,
    thread: String,
    slot: DriverSlot,
) -> Result<(), ApiError> {
    // Keep one connection generation across this Run, including waits and tool loops.
    let clients = control.store.clients.snapshot();
    let boundary = control.boundary(&thread).await;
    let mut overflow = OverflowBudget::default();
    let mut retry_budget = retry::RetryBudget::default();
    let mut instructions = Instructions::default();
    loop {
        if slot.stop.is_cancelled() {
            drain_and_stop(&control, &slot.run_id, &thread).await;
            return Ok(());
        }
        let _ = slot.nudge.notified().now_or_never();
        let boundary_guard = tokio::select! {biased;_=slot.stop.cancelled()=>{drain_and_stop(&control,&slot.run_id,&thread).await;return Ok(());},guard=boundary.lock()=>guard};
        let completed_job = {
            let join_notified = slot.proactive_join_done.clone().notified_owned();
            tokio::pin!(join_notified);
            join_notified.as_mut().enable();
            let mut job = slot.proactive_job.lock().await;
            if matches!(*job, proactive::ThreadJob::Joining) {
                drop(job);
                drop(boundary_guard);
                tokio::select! {
                    _ = slot.stop.cancelled() => { drain_and_stop(&control, &slot.run_id, &thread).await; return Ok(()); },
                    _ = join_notified => {}
                }
                continue;
            }
            job.take_finished()
        };
        if let Some(mut job) = completed_job {
            let joined = job.join().await;
            control
                .live
                .prune_idle_background(&thread, &slot.proactive_job)
                .await;
            match joined {
                Ok(Ok(proactive::ProactiveOutcome::Finished { path_changed: true })) => {
                    instructions.clear();
                    drop(boundary_guard);
                    continue;
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(invalid("background proactive compaction task panicked")),
            }
        }
        if slot.stop.is_cancelled() {
            drain_and_stop(&control, &slot.run_id, &thread).await;
            return Ok(());
        }
        super::turn_interrupt::acknowledge(&control, &thread, &slot.run_id).await?;
        let snapshot =
            match next_with_clients(&control, &thread, &slot.run_id, clients.clone()).await? {
                Next::Recheck => continue,
                Next::Drain => {
                    drop(boundary_guard);
                    control.live.cancel_join_proactive_job(&thread, &slot).await;
                    drain_and_stop(&control, &slot.run_id, &thread).await;
                    return Ok(());
                }
                Next::Done => return Ok(()),
                Next::Idle => {
                    instructions.clear();
                    drop(boundary_guard);
                    tokio::select! {_=slot.stop.cancelled()=>{},_=slot.nudge.notified()=>{}};
                    continue;
                }
                Next::Ready(snapshot) => snapshot,
            };
        let turn_binding = slot.bind_turn(&control, &snapshot.turn).await?;
        if turn_binding.cancel.is_cancelled() {
            continue;
        }
        let mut boundary_guard = boundary_guard;
        let mut request_slot = slot.clone();
        request_slot.stop = turn_binding.cancel.clone();
        overflow.observe(&snapshot.turn, snapshot.generation);
        retry_budget.observe(&snapshot.turn, snapshot.generation);
        if let Err(error) = control.gc.require_capacity().await {
            return fail_turn(&control, &snapshot, error).await;
        }
        // Safe request boundary: tool groups are complete and no request is in
        // flight. A reported input over the threshold compacts here, one step
        // after the large request; the new snapshot is then read again.
        let snapshot = snapshot;
        let join_notified = slot.proactive_join_done.clone().notified_owned();
        tokio::pin!(join_notified);
        join_notified.as_mut().enable();
        let mut jobs = slot.proactive_job.lock().await;
        if matches!(*jobs, proactive::ThreadJob::Joining) {
            drop(jobs);
            drop(boundary_guard);
            tokio::select! {
                _ = slot.stop.cancelled() => { drain_and_stop(&control, &slot.run_id, &thread).await; return Ok(()); },
                _ = turn_binding.cancel.cancelled() => continue,
                _ = join_notified => {}
            }
            continue;
        }
        if matches!(*jobs, proactive::ThreadJob::Idle)
            && let Some(plan) = proactive::preflight(&control, &snapshot).await?
        {
            drop(boundary_guard);
            let job = proactive::spawn_background(
                control.clone(),
                *snapshot.clone(),
                slot.clone(),
                boundary.clone(),
                slot.stop.clone(),
                plan,
            );
            *jobs = proactive::ThreadJob::Retained(job);
            if let proactive::ThreadJob::Retained(job) = &mut *jobs {
                job.wait_started().await?;
            }
            drop(jobs);
            boundary_guard = tokio::select! {biased;
                _=slot.stop.cancelled()=>{drain_and_stop(&control,&slot.run_id,&thread).await;return Ok(());},
                _=turn_binding.cancel.cancelled()=>continue,
                guard=boundary.lock()=>guard,
            };
        } else {
            drop(jobs);
        }
        let refreshed = instructions
            .refresh(&control, snapshot, &request_slot)
            .await;
        if turn_binding.cancel.is_cancelled() {
            continue;
        }
        let Some(snapshot) = refreshed? else {
            continue;
        };
        let rate_client = match model_client(&control, &snapshot) {
            Ok(client) => client,
            Err(error) => return fail_turn(&control, &snapshot, error).await,
        };
        let Some(connection) = clients.connections.get(&snapshot.profile) else {
            return fail_turn(
                &control,
                &snapshot,
                invalid("Model connection not configured"),
            )
            .await;
        };
        let mut caller = ConversationCaller {
            control: &control,
            slot: &request_slot,
            client: &rate_client,
            snapshot: *snapshot,
            instruction: instructions.body(),
            output_limit: None,
            context_tail: None,
            context_start_gate: turn_binding.start_gate.clone(),
            boundary: crate::compaction::boundary::Boundary::new(&boundary, boundary_guard),
        };
        let mut partial_attempts = Vec::new();
        let result = execute_request(
            RequestScope {
                database: &control.store.database,
                client: &rate_client,
                connection,
                kind: AttemptKind::Conversation,
                run: Some(&slot.run_id),
                adapt_after_transient: connection.auto_trim_on_disconnect,
            },
            &mut caller,
            &mut retry_budget,
            &turn_binding.cancel,
            &mut partial_attempts,
        )
        .await;
        let output_limit = caller.output_limit;
        let snapshot = caller.snapshot;
        let mut request_boundary = caller.boundary;
        let (attempt, outcome, prepared, adaptation, decision) = match result {
            Ok(Execution::Finished {
                attempt,
                outcome,
                context,
                failure_decision,
            }) => (attempt, *outcome, Some(context), false, failure_decision),
            Ok(Execution::AdaptationSuggested {
                attempt,
                failure,
                decision,
            }) => (attempt, Err(*failure), None, true, Some(decision)),
            Ok(Execution::InputChanged | Execution::Fenced | Execution::RunCeilingReached) => {
                continue;
            }
            Ok(Execution::BudgetExhausted) => {
                return fail_turn(
                    &control,
                    &snapshot,
                    ApiError::new(ErrorCode::IoFailed, "Model retry budget exhausted"),
                )
                .await;
            }
            Err(error) if error.code == ErrorCode::OperationConflict => continue,
            Err(error) => {
                if slot.stop.is_cancelled() {
                    drain_and_stop(&control, &slot.run_id, &thread).await;
                }
                return Err(error);
            }
        };
        if turn_binding.cancel.is_cancelled() || slot.stop.is_cancelled() {
            match (outcome, &output_limit) {
                (Ok(_), _) | (Err(_), Some(_)) => {
                    let current = snapshot.clone();
                    let saved = attempt.clone();
                    let rejected = control
                        .store
                        .database
                        .call(move |db| {
                            let tx = db.transaction()?;
                            let rejected = reject_adoption(&tx, &current, &saved)?;
                            tx.commit()?;
                            Ok(rejected)
                        })
                        .await
                        .map_err(storage_error)?;
                    if rejected {
                        control.record_attempt_end(
                            &snapshot.thread,
                            &attempt,
                            crate::protocol::EventAttemptOutcome::NotAdopted,
                            output_limit.clone(),
                        );
                    }
                }
                (Err(failure), None) if !slot.stop.is_cancelled() => {
                    let body = model::request_path::interrupted_failure_body(&attempt, &failure);
                    record_interrupted_outcome(&control, &snapshot, &body).await?;
                }
                (Err(_), None) => {}
            }
            if slot.stop.is_cancelled() {
                drain_and_stop(&control, &slot.run_id, &thread).await;
                return Ok(());
            }
            control.changed(&thread);
            continue;
        }
        let response = match (outcome, output_limit) {
            (Ok(value), _) => {
                retry_budget.clear_usage_limit_retry_bypass();
                if let Some(observation) = &value.stream_observation {
                    // Fixed, bounded classifications only. Adoption owns the received audit data.
                    for difference in &observation.differences {
                        control.diagnostics.record_model_stream_difference(
                            crate::diagnostics::DiagnosticIds {
                                attempt_id: None,
                                thread_id: Some(snapshot.thread.clone()),
                                run_id: Some(snapshot.run.clone()),
                                turn_id: Some(snapshot.turn.clone()),
                                execution_id: None,
                            },
                            &attempt,
                            difference,
                        );
                    }
                    for field in &observation.delta_differences {
                        control.diagnostics.record_model_stream_delta_difference(
                            crate::diagnostics::DiagnosticIds {
                                attempt_id: None,
                                thread_id: Some(snapshot.thread.clone()),
                                run_id: Some(snapshot.run.clone()),
                                turn_id: Some(snapshot.turn.clone()),
                                execution_id: None,
                            },
                            &attempt,
                            *field,
                        );
                    }
                }
                value
            }
            (Err(failure), Some(limit)) => model::ModelResponse::truncated(failure, limit),
            (Err(failure), None) => {
                control.diagnostics.record_model_failure(
                    crate::diagnostics::DiagnosticIds {
                        attempt_id: None,
                        thread_id: Some(snapshot.thread.clone()),
                        run_id: Some(snapshot.run.clone()),
                        turn_id: Some(snapshot.turn.clone()),
                        execution_id: None,
                    },
                    &attempt,
                    &failure.error,
                );
                {
                    let mut job = request_slot.proactive_job.lock().await;
                    retry::retry_or_recover(
                        &control,
                        &snapshot,
                        &request_slot,
                        attempt,
                        failure,
                        adaptation,
                        decision.expect("non-cancelled failure has an executor decision"),
                        &mut retry_budget,
                        &mut overflow,
                        &mut job,
                        &mut request_boundary,
                        partial_attempts,
                    )
                    .await?;
                }
                drop(request_boundary);
                control
                    .live
                    .prune_idle_background(&thread, &slot.proactive_job)
                    .await;
                continue;
            }
        };
        let prepared = prepared.expect("successful response has a committed attempt context");
        let _boundary_guard = request_boundary.into_guard();
        let group = if response.calls.is_empty() {
            None
        } else {
            Some(id())
        };
        let may_execute = adopt_response(
            &control,
            &snapshot,
            &attempt,
            &response,
            prepared.clone(),
            group.clone(),
            partial_attempts,
        )
        .await?;
        if !may_execute {
            continue;
        }
        retry_budget.succeeded();
        overflow.succeeded();
        drop(prepared);
        let Some(group) = group else {
            continue;
        };
        run_tool_group(&control, &snapshot, &slot, &group, response.calls).await?;
    }
}

struct ConversationCaller<'a, 'b> {
    control: &'a Arc<Control>,
    slot: &'a DriverSlot,
    client: &'a HttpModelClient,
    snapshot: Snapshot,
    instruction: Option<&'a Arc<str>>,
    output_limit: Option<crate::protocol::AttemptOutputLimit>,
    context_tail: Option<Option<Value>>,
    context_start_gate: Arc<Mutex<bool>>,
    boundary: crate::compaction::boundary::Boundary<'b>,
}

impl RequestCaller for ConversationCaller<'_, '_> {
    type Context = Arc<crate::attachments::PreparedAttachments>;

    fn pause<'a, T: Send + 'a>(
        &'a mut self,
        cancel: &'a CancellationToken,
        wait: impl Future<Output = T> + Send + 'a,
    ) -> futures_util::future::BoxFuture<'a, Option<T>> {
        Box::pin(async move { self.boundary.pause(cancel, wait).await.ok() })
    }
    fn release_pins(&mut self) {
        self.snapshot.input.take();
    }

    async fn prepare(
        &mut self,
        waited: bool,
    ) -> Result<Option<PreparedInput<Self::Context>>, ApiError> {
        if waited || self.snapshot.input.is_none() {
            let Next::Ready(refreshed) = next_with_clients(
                self.control,
                &self.snapshot.thread,
                &self.slot.run_id,
                self.snapshot.clients.clone(),
            )
            .await?
            else {
                return Ok(None);
            };
            if refreshed.turn != self.snapshot.turn
                || refreshed.generation != self.snapshot.generation
                || refreshed.selection != self.snapshot.selection
                || refreshed.profile_revision != self.snapshot.profile_revision
            {
                return Ok(None);
            }
            self.snapshot = *refreshed;
        }
        if self.context_tail.is_none() {
            let cancel = &self.slot.stop;
            let result = self
                .boundary
                .pause_owned(
                    cancel,
                    context_tail::collect(
                        self.control,
                        &self.snapshot,
                        cancel,
                        self.context_start_gate.clone(),
                    ),
                )
                .await;
            let Ok(tail) = result else {
                return Ok(None);
            };
            self.context_tail = Some(tail?);
        }
        prepare_attempt(
            self.control,
            &mut self.snapshot,
            self.slot,
            self.instruction,
            self.client,
            self.context_tail.as_ref().and_then(Option::as_ref),
        )
        .await
    }

    fn unsuccessful_attempt_committed(
        &mut self,
        attempt: &str,
        output_limit: Option<crate::protocol::AttemptOutputLimit>,
    ) {
        if let Some(limit) = output_limit {
            self.output_limit = Some(limit);
        } else {
            self.control.record_attempt_end(
                &self.snapshot.thread,
                attempt,
                crate::protocol::EventAttemptOutcome::NotAdopted,
                None,
            );
        }
    }

    fn attempt_changed(&self) {
        self.control.changed(&self.snapshot.thread);
    }

    async fn observe<'a>(
        &'a mut self,
        attempt: &'a str,
    ) -> Option<model::request_path::Observer<'a>> {
        self.attempt_changed();
        Some(
            stream_observer(
                self.control,
                &self.snapshot.thread,
                attempt,
                self.snapshot
                    .input
                    .as_ref()
                    .and_then(super::replay::ReplayLease::selected_tail),
            )
            .await,
        )
    }
}

async fn fail_turn<T>(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    error: ApiError,
) -> Result<T, ApiError> {
    recover(control, snapshot, error.clone(), None).await?;
    Err(error)
}

/// A retry of the same Turn reuses its durable snapshot, including the explicit
/// observation that AGENTS.md was absent. New Turns alone re-observe the file.
async fn turn_instructions(
    control: &Control,
    snapshot: &Snapshot,
) -> Result<Option<Arc<str>>, ApiError> {
    let turn = snapshot.turn.clone();
    let saved=control.store.database.call(move|db|Ok(db.query_row("SELECT s.body,s.observed_size,s.content_hash FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[turn],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<i64>>(1)?,row.get::<_,String>(2)?))).optional()?)).await.map_err(storage_error)?;
    if let Some((body, size, hash)) = saved {
        return Ok(size.map(|_| Arc::from(crate::instructions::saved_body(body, &hash))));
    }
    let (prepared, warnings) = control
        .store
        .instructions
        .prepare_with_extra(
            snapshot.cwd.clone(),
            snapshot.agents.clone(),
            control.store.config.current().extra_instructions,
        )
        .await?;
    let body = prepared.as_ref().map(|value| {
        Arc::from(crate::instructions::saved_body(
            value.body.to_string(),
            &value.hash,
        ))
    });
    let turn = snapshot.turn.clone();
    let source_path = snapshot.agents.clone();
    control.store.database.call(move|db| {
        let tx=db.transaction()?;
        let snapshot_id=id();
        let (path,hash,body,size)=if let Some(value)=prepared {
            (value.source_path.to_string_lossy().into_owned(),value.hash,value.body.to_string(),Some(value.size as i64))
        } else {
            (source_path.to_string_lossy().into_owned(),"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),String::new(),None)
        };
        let (thread,run):(String,String)=tx.query_row("SELECT thread_id,run_id FROM turn WHERE id=?1",[&turn],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let previous:Option<(String,String,Option<i64>,String)>=tx.query_row("SELECT s.source_path,s.content_hash,s.observed_size,s.id FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.thread_id=?1 AND t.id<>?2 ORDER BY t.created_at DESC,t.rowid DESC LIMIT 1",params![thread,turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        // An unchanged observation (same path, hash and size) shares the previous
        // Turn's immutable row; history GC removes rows no Turn references.
        let reused=previous.as_ref().filter(|(p,h,s,_)|*p==path&&*h==hash&&*s==size).map(|(_,_,_,id)|id.clone());
        if reused.is_none() {
            use super::notification::{Fact,Observation};
            let observation=if size.is_none() {Observation::Removed} else if previous.is_none() {Observation::FirstSeen} else {Observation::Updated};
            super::context::record_fact(&tx,&thread,"instructions_observed",json!({"kind":"shellboy","run_id":run}),json!({"before":previous.map(|(path,hash,size,_)|json!({"path":path,"hash":hash,"present":size.is_some()})),"after":{"path":path,"hash":hash,"present":size.is_some()}}),&now(),Fact::InstructionsObserved{observation})?;
        }
        let snapshot_id=match reused {
            Some(previous)=>previous,
            None=>{
                tx.execute("INSERT INTO instruction_snapshot(id,source_path,content_hash,body,observed_size,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![snapshot_id,path,hash,body,size,now()])?;
                snapshot_id
            }
        };
        let saved = tx.execute("UPDATE turn SET instruction_snapshot_id=?2 WHERE id=?1 AND instruction_snapshot_id IS NULL",params![turn,snapshot_id])?;
        if saved != 0 {
            let warnings = (!warnings.is_empty()).then(|| json!(warnings).to_string());
            tx.execute("UPDATE run SET instruction_warnings_json=?2 WHERE id=?1", params![run,warnings])?;
        }
        tx.commit()?;
        Ok(())
    }).await.map_err(storage_error)?;
    Ok(body)
}

#[cfg(test)]
async fn next(control: &Arc<Control>, thread: &str, run_id: &str) -> Result<Next, ApiError> {
    next_with_clients(control, thread, run_id, control.store.clients.snapshot()).await
}
async fn next_with_clients(
    control: &Arc<Control>,
    thread: &str,
    run_id: &str,
    clients: Arc<model::http::ClientSnapshot>,
) -> Result<Next, ApiError> {
    let thread_id = thread.to_owned();
    let run_target = run_id.to_owned();
    let thread = thread_id.clone();
    let run_id = run_target.clone();
    let index = control.index.clone();
    let replay = control.replay.clone();
    let mut config = (*control.store.config).clone();
    config.connections = clients.connections.clone();
    let shutting_down = control.shell.start_gate();
    let wakes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let committed_wakes = wakes.clone();
    let wait_wakes = control.waits.keyed.clone();
    let result = control.store.database.call(move|db|{
        let tx=db.transaction()?;
        let mut keys = Vec::new();
        let status:Option<(String,Option<String>)>=tx.query_row("SELECT state,stop_reason FROM run WHERE thread_id=?1 AND id=?2",params![thread,run_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        match status.as_ref().map(|(state, reason)| (state.as_str(), reason.as_deref())) {
            Some(("stopping", Some(super::state::MODEL_REQUEST_CEILING_REACHED))) => return Ok(Next::Drain),
            Some(("live", _)) => {},
            _ => return Ok(Next::Done),
        }
        let run=run_id;
        let interrupted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait') AND interrupt_requested_at IS NOT NULL)",[&run],|r|r.get(0))?;
        if interrupted {
            // A failed earlier driver can leave an adopted group unfinished.
            // Acknowledgement cannot pass it: wait for progress/stop rather
            // than filling the shared DB lane with an unbounded recheck loop.
            let blocked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn u JOIN tool_group g ON g.turn_id=u.id WHERE u.run_id=?1 AND u.state IN ('preparing','running','waiting','recovery_wait') AND u.interrupt_requested_at IS NOT NULL AND g.state<>'completed')",[&run],|r|r.get(0))?;
            return Ok(if blocked {Next::Idle} else {Next::Recheck});
        }
        let mut turn:Option<String>=tx.query_row("SELECT id FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting')",[&run],|r|r.get(0)).optional()?;
        let recovering:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state='recovery_wait')",[&run],|r|r.get(0))?;
        if recovering{return Ok(Next::Done);}
        let attachment_count = super::attachment_delivery::apply_pending_tx(&tx,&thread,&now()).map_err(crate::storage::StorageError::from)?;
        let attachment_blocked=super::attachment_delivery::has_pending_tx(&tx,&thread).map_err(crate::storage::StorageError::from)?;
        // A foreign shell group can still be running. Do not let a later human
        // receipt overtake its accepted addition. Roll back partial adoption;
        // the originating group's completion nudges this driver again.
        if attachment_blocked { return Ok(Next::Idle); }
        let mut events:Vec<(String,String)>=tx.prepare("SELECT e.id,json_object('event_id',e.id,'kind',e.kind,'payload',json(e.payload_ref_json)) FROM event e JOIN delivery d ON d.event_id=e.id WHERE d.recipient_id=?1 AND d.state='pending' AND e.kind<>'context_changed' ORDER BY e.created_at,e.id")?.query_map([&run],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        if turn.is_none()&&(!events.is_empty()||attachment_count>0){turn=Some(super::state::create_turn_tx(&tx,&thread,&run,&now(),&now())?);}
        if turn.is_none() {
            let owned:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping'))",[&run],|r|r.get(0))?;
            if owned{return Ok(Next::Idle);}
            // During shutdown, waiting Revisit messages are handed off at the next start.
            if shutting_down.load(std::sync::atomic::Ordering::SeqCst) && super::revisit::has_pending(&tx,&thread)? { return Ok(Next::Done); }
            turn=super::revisit::release_tx(&tx,&thread,&run,&now())?;
            if turn.is_some() { keys.push(Key::CallerInput(thread.clone())); }
            if turn.is_none() && !shutting_down.load(std::sync::atomic::Ordering::SeqCst)
                && let Some(event)=super::reply::complete_tx(&tx,&thread,&run,&now(),&mut wakes.lock().unwrap(), &mut keys)? {
                turn=Some(super::state::create_turn_tx(&tx,&thread,&run,&now(),&now())?);
                events.push(tx.query_row("SELECT id,json_object('event_id',id,'kind',kind,'payload',json(payload_ref_json)) FROM event WHERE id=?1",[event],|r|Ok((r.get(0)?,r.get(1)?)))?);
            }
            if turn.is_none() {
                let revision = history::selection_revision(&tx, &thread, history::ThreadScope::Live)?;
                super::message_delivery::append_admitted_tx(&tx,&thread,revision as u64,&[],&now())?;
                super::state::complete_idle_run(&tx, &run)?;
                tx.commit()?;
                keys.extend([super::wait::wakes::Key::Run(run), super::wait::wakes::Key::ThreadSettled(thread)]);
                wait_wakes.publish(keys);
                return Ok(Next::Done);
            }
        }
        let turn=turn.expect("an active or Revisit Turn was established");
        index.record_context_observations_tx(&tx,&thread,&now()).map_err(crate::storage::StorageError::from)?;
        // Context-only observations have no delivery until an actual Turn is
        // already runnable: settings never wake background/recovery waits.
        tx.execute("INSERT OR IGNORE INTO delivery(event_id,recipient_id,state) SELECT e.id,?2,'pending' FROM event e WHERE e.target_thread_id=?1 AND e.kind='context_changed' AND e.id NOT GLOB 'reply-admission-?*' AND e.target_run_id IS NULL AND json_extract(e.payload_ref_json,'$.kind') IS NOT 'interruption_capture_row' AND json_extract(e.payload_ref_json,'$.kind') IS NOT 'interruption_capture_header' AND (json_extract(e.payload_ref_json,'$.kind') IS NOT 'agents_reference' OR json_extract(e.payload_ref_json,'$.change.excluded_turn_id') IS NOT ?3) AND NOT EXISTS(SELECT 1 FROM delivery d WHERE d.event_id=e.id AND d.state='processed')",params![thread,run,turn])?;
        super::reply::offer_unadmitted_outcomes_tx(&tx,&thread,&run)?;
        let events:Vec<(String,String)>=tx.prepare("SELECT e.id,json_object('event_id',e.id,'kind',e.kind,'payload',json(e.payload_ref_json)) FROM event e JOIN delivery d ON d.event_id=e.id WHERE d.recipient_id=?1 AND d.state='pending' AND NOT (e.kind='context_changed' AND e.id GLOB 'reply-admission-?*') AND json_extract(e.payload_ref_json,'$.kind') IS NOT 'interruption_capture_row' AND json_extract(e.payload_ref_json,'$.kind') IS NOT 'interruption_capture_header' AND (json_extract(e.payload_ref_json,'$.kind') IS NOT 'agents_reference' OR json_extract(e.payload_ref_json,'$.change.excluded_turn_id') IS NOT ?2) AND (e.kind<>'context_changed' OR NOT EXISTS(SELECT 1 FROM delivery done WHERE done.event_id=e.id AND done.state='processed')) ORDER BY e.created_at,e.id")?.query_map(params![run,turn],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        let mut snapshot=tx.query_row("SELECT u.generation,t.selection_revision,t.profile_revision,t.profile_id,t.model_id,t.effort,t.reasoning_epoch,t.cwd,COALESCE((SELECT s.source_path FROM instruction_snapshot s WHERE s.id=u.instruction_snapshot_id),t.agents_path),t.max_tokens FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=?1",[&turn],|r|Ok(Snapshot{clients:clients.clone(),thread:thread.clone(),run:run.clone(),turn:turn.clone(),generation:r.get(0)?,selection:r.get(1)?,profile_revision:r.get(2)?,profile:r.get(3)?,model:r.get(4)?,effort:r.get(5)?,max_tokens:r.get::<_,Option<i64>>(9)?.map(|v|v as u64),epoch:r.get(6)?,cwd:PathBuf::from(r.get::<_,String>(7)?),agents:PathBuf::from(r.get::<_,String>(8)?),cache_comparison_response_id:None,cache_diagnostics_enabled:false,input_bytes:None,input:None,receipts:vec![],events:events.iter().map(|v|v.0.clone()).collect()}))?;
        if config.connections.get(&snapshot.profile).is_some_and(|c| c.prompt_cache_diagnostics) {
            snapshot.cache_comparison_response_id = model::cache_diagnostics::last_response_id(&tx, &thread)?;
        }
        let (receipts, plan)=super::message_delivery::plan_turn_input(&tx,&thread,&turn,&now())?;
        snapshot.receipts=receipts;
        super::message_delivery::append_admission_plan_tx(&tx,&thread,snapshot.selection as u64,&plan,&now())?;
        for (event,payload)in &events {
            notification_admission::admit_notification_tx(&tx,&thread,&run,&turn,event,payload,&now())?;
        }
        // #433: this Run now holds any answer or outcome a reopened duty waited for.
        super::reply::claim_reopened_tx(&tx,&thread,&run,&now())?;
        // #183: after the delivered events, so a change notice and the new
        // context line arrive together. Only once the Turn's AGENTS reference
        // is fixed by its instruction snapshot; requests always follow that.
        let instructions_fixed:bool=tx.query_row("SELECT instruction_snapshot_id IS NOT NULL FROM turn WHERE id=?1",[&turn],|r|r.get(0))?;
        if instructions_fixed { snapshot.ensure_context_tx(&tx,&now())?; }
        snapshot.selection=history::selection_revision(&tx, &thread, history::ThreadScope::Any)?;
        snapshot.input=Some(replay.load_and_commit(tx,&thread)?);
        wait_wakes.publish(keys);Ok(Next::Ready(Box::new(snapshot)))
    }).await.map_err(storage_error);
    if result.is_ok() {
        let wakes = std::mem::take(&mut *committed_wakes.lock().unwrap());
        for thread in wakes {
            control.changed(&thread);
            control.kick_quiet(thread).await;
        }
        // The commit may have created a Turn and applied queued input: announce it now,
        // not at the later admit_attempt (compaction and the rate gate can delay that).
        if matches!(&result, Ok(Next::Ready(_))) {
            control.changed(&thread_id);
        } else {
            control.changed.notify_waiters();
        }
    }
    if let Err(error) = &result
        && !error.is_storage_failure()
        && error.code != ErrorCode::OperationConflict
    {
        recover_preparation(control, thread_id, run_target, error.clone()).await?;
    }
    result
}

/// Preparation can fail before a Snapshot exists. Preserve the failed source,
/// and make that attempt explicit rather than re-kicking the same live Run.
async fn recover_preparation(
    control: &Arc<Control>,
    thread: String,
    run: String,
    error: ApiError,
) -> Result<(), ApiError> {
    let target_thread = thread.clone();
    let target_run = run.clone();
    let wait_wakes = control.waits.keyed.clone();
    let target = control.store.database.call(move |db| {
        let tx = db.transaction()?;
        let live: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND thread_id=?2 AND state='live')",params![run,thread],|row|row.get(0))?;
        if !live { return Ok(None); }
        let recovering: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state='recovery_wait')",[&run],|row|row.get(0))?;
        if recovering { return Ok(None); }
        let active: Option<(String,i64)> = tx.query_row("SELECT id,generation FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting')",[&run],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        let (turn,generation) = match active {
            Some(target) => target,
            None => {
                let turn = super::state::create_turn_tx(&tx,&thread,&run,&now(),&now())?;
                (turn,0)
            }
        };
        let recovered = recover_turn_tx(&tx,&thread,&run,&turn,generation,&error)?;
        tx.commit()?;
        if recovered { wait_wakes.publish(Key::recovery(thread, run, turn.clone())); }
        Ok(recovered.then_some(turn))
    }).await.map_err(storage_error)?;
    if let Some(turn) = target {
        control
            .diagnostics
            .record_preparation_failure(crate::diagnostics::DiagnosticIds {
                attempt_id: None,
                thread_id: Some(target_thread.clone()),
                run_id: Some(target_run.clone()),
                turn_id: Some(turn),
                execution_id: None,
            });
        control.changed_with_recovery(&target_thread, true);
    }
    // Keep the original typed failure as the one driver error. The recovery
    // reason is persisted above; the caller must not silently report success.
    Ok(())
}

fn save_partial_attempts(
    tx: &rusqlite::Transaction<'_>,
    snapshot: &Snapshot,
    attempts: &[String],
) -> Result<(), crate::storage::StorageError> {
    for attempt in attempts {
        let body: String = tx.query_row(
            "SELECT error_json FROM model_attempt WHERE id=?1 AND turn_id=?2 AND generation=?3",
            params![attempt, snapshot.turn, snapshot.generation],
            |row| row.get(0),
        )?;
        let body = serde_json::from_str(&body)
            .map_err(|e| crate::storage::StorageError::Schema(e.to_string()))?;
        save_received_failure(tx, &snapshot.turn, &body)?;
    }
    Ok(())
}

fn reject_adoption(
    tx: &rusqlite::Transaction<'_>,
    snapshot: &Snapshot,
    attempt: &str,
) -> rusqlite::Result<bool> {
    Ok(tx.execute("UPDATE model_attempt SET input_manifest_json=json_set(input_manifest_json,'$.adoption',json_object('outcome','not_adopted','reason','turn_fenced')) WHERE id=?1 AND turn_id=?2 AND generation=?3 AND (state IN ('completed','cancelled') OR (state='incomplete' AND json_extract(error_json,'$.error.kind')='output_limit')) AND json_type(input_manifest_json,'$.adoption') IS NULL",params![attempt,snapshot.turn,snapshot.generation])? == 1)
}

fn save_interrupted_outcome(
    tx: &rusqlite::Transaction<'_>,
    snapshot: &Snapshot,
    body: &Value,
) -> Result<bool, crate::storage::StorageError> {
    // Interrupt owns a single generation increment; this audit is for that exact successor only.
    let interrupted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE id=?1 AND generation=?2+1 AND interrupt_requested_at IS NOT NULL)", params![snapshot.turn,snapshot.generation], |row| row.get(0))?;
    if interrupted {
        save_received_failure(tx, &snapshot.turn, body)?;
    }
    Ok(interrupted)
}

async fn record_interrupted_outcome(
    control: &Control,
    snapshot: &Snapshot,
    body: &Value,
) -> Result<(), ApiError> {
    let snapshot = snapshot.clone();
    let body = body.clone();
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            save_interrupted_outcome(&tx, &snapshot, &body)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(storage_error)
}

fn live_outcome_generation(
    tx: &rusqlite::Transaction<'_>,
    snapshot: &Snapshot,
) -> rusqlite::Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id WHERE u.id=?1 AND u.generation=?2 AND u.state IN ('preparing','running','waiting') AND r.state='live')", params![snapshot.turn,snapshot.generation], |row| row.get(0))
}

async fn recover(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    error: ApiError,
    attempt: Option<String>,
) -> Result<(), ApiError> {
    recover_with_observation(control, snapshot, error, attempt, None, Vec::new()).await
}
async fn recover_with_observation(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    error: ApiError,
    attempt: Option<String>,
    observation: Option<trim_observation::DriverTrimObservation>,
    partial_attempts: Vec<String>,
) -> Result<(), ApiError> {
    let error = crate::config_setup::enrich(
        &control.store.config,
        Some(&snapshot.profile),
        Some(&snapshot.model),
        error,
    );
    // An attempt failure was already classified at the typed ModelError boundary.
    if attempt.is_none() {
        control.diagnostics.record(
            crate::diagnostics::DiagnosticEvent::ModelFailure,
            crate::diagnostics::DiagnosticIds {
                attempt_id: None,
                thread_id: Some(snapshot.thread.clone()),
                run_id: Some(snapshot.run.clone()),
                turn_id: Some(snapshot.turn.clone()),
                execution_id: None,
            },
        );
    }
    let thread_id = snapshot.thread.clone();
    let snapshot = snapshot.clone();
    let wait_wakes = control.waits.keyed.clone();
    let recovered = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let recovered = recover_turn_tx(
                &tx,
                &snapshot.thread,
                &snapshot.run,
                &snapshot.turn,
                snapshot.generation,
                &error,
            )?;
            if !recovered {
                return Ok(false);
            }
            save_partial_attempts(&tx, &snapshot, &partial_attempts)?;
            if let Some(attempt) = attempt {
                if let Some(observation) = &observation {
                    observation.persist_trim(&tx, &attempt)?;
                }
                let saved: String = tx.query_row("SELECT error_json FROM model_attempt WHERE id=?1 AND turn_id=?2 AND generation=?3", params![attempt,snapshot.turn,snapshot.generation], |r|r.get(0))?;
                let body = serde_json::from_str(&saved).map_err(|e| crate::storage::StorageError::Schema(e.to_string()))?;
                save_received_failure(&tx, &snapshot.turn, &body)?;
            }
            tx.commit()?;
            wait_wakes.publish(Key::recovery(snapshot.thread, snapshot.run, snapshot.turn));
            Ok(recovered)
        })
        .await
        .map_err(storage_error)?;
    control.changed_with_recovery(&thread_id, recovered);

    Ok(())
}
pub(super) async fn stop_without_driver(control: &Arc<Control>, run_id: &str) -> bool {
    match try_stop_without_driver(control, run_id).await {
        Ok(()) => true,
        Err(error) => {
            control.record_stop_settlement_failure(run_id, error).await;
            false
        }
    }
}

async fn try_stop_without_driver(control: &Arc<Control>, run_id: &str) -> Result<(), StorageError> {
    loop {
        let run = run_id.to_owned();
        let target: Option<(String, String)> = control
            .store
            .database
            .call(move |db| {
                Ok(db
                    .query_row("SELECT thread_id,state FROM run WHERE id=?1", [run], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?)
            })
            .await?;
        let Some((thread, state)) = target else {
            return Ok(());
        };
        if state != "stopping" {
            return Ok(());
        }

        // Prejoin an already-inactive exact Run before taking its Thread boundary.
        // The ownership state is checked again below at the actual boundary cut.
        control
            .live
            .cancel_join_inactive_proactive_job_for_run(&thread, run_id)
            .await;
        let boundary = control.boundary(&thread).await;
        let boundary_guard = boundary.lock().await;
        let state_run = run_id.to_owned();
        let still_stopping = control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND state='stopping')",
                    [state_run],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await?;
        if !still_stopping {
            drop(boundary_guard);
            return Ok(());
        }

        match control
            .live
            .stopped_run_boundary_state(&thread, run_id)
            .await
        {
            super::live::StoppedRunBoundary::MatchingDriverActive => {
                // The current Driver owns this Run's stop; let it enter its own drain.
                drop(boundary_guard);
                return Ok(());
            }
            super::live::StoppedRunBoundary::InactiveJobNeedsJoin => {
                // Boundary-dependent work must reacquire before its slot can be joined.
                drop(boundary_guard);
                control
                    .live
                    .cancel_join_inactive_proactive_job_for_run(&thread, run_id)
                    .await;
                continue;
            }
            super::live::StoppedRunBoundary::NoMatchingDriver
            | super::live::StoppedRunBoundary::InactiveRunClear => {
                try_drain_and_stop(control, run_id, None).await?;
                return Ok(());
            }
        }
    }
}

async fn drain_and_stop(control: &Arc<Control>, run_id: &str, thread: &str) {
    if let Err(error) = try_drain_and_stop(control, run_id, Some(thread)).await {
        control.record_stop_settlement_failure(run_id, error).await;
    }
}

/// Freeze this Run, drain its shells, then record terminal state only after
/// every owned shell has released its live handle.
async fn try_drain_and_stop(
    control: &Arc<Control>,
    run_id: &str,
    current_thread: Option<&str>,
) -> Result<(), StorageError> {
    let run_id = run_id.to_owned();
    let preserve_usage_waits = control.store.config.recovery.auto_retry_usage_limit;
    let stop = control.stop.clone();
    let owned = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            if preserve_usage_waits
                && stop.is_cancelled()
                && super::usage_limit_retry::preserve_run(&tx, &run_id)?
            {
                return Ok(Vec::new());
            }
            let owned: Vec<(String, String)> = tx
                .prepare("SELECT id,thread_id FROM run WHERE id=?1")?
                .query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (run, _) in &owned {
                super::state::admit_run_stop_tx(
                    &tx,
                    run,
                    super::state::RunStopAdmission::DriverDrain,
                )?;
            }
            tx.commit()?;
            Ok(owned)
        })
        .await?;
    if owned.is_empty() {
        return Ok(());
    }
    let runs: std::collections::HashSet<String> =
        owned.iter().map(|(run, _)| run.clone()).collect();
    control.live.cancel_owned(&runs, current_thread).await;
    loop {
        let notified = control.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !control.live.has_owned(&runs, current_thread).await {
            break;
        }
        notified.await;
    }
    let stopping_for_shutdown = control.stop.is_cancelled();
    let wait_wakes = control.waits.keyed.clone();
    let wakes = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let mut keys = Vec::new();
            super::state::stop_drained_runs(&tx, &owned, stopping_for_shutdown, &mut keys)?;
            let mut wakes = Vec::new();
            for (run, _) in &owned {
                wakes.extend(super::reply::terminal_tx(&tx, run, &now(), &mut keys)?);
            }
            tx.commit()?;
            wait_wakes.publish(keys);
            Ok(wakes)
        })
        .await?;
    for thread in wakes {
        control.changed(&thread);
        control.kick_quiet(thread).await;
    }
    Ok(())
}

async fn save_tool_result(
    control: &Control,
    snapshot: &Snapshot,
    group: &str,
    call: &str,
    mut result: Value,
) -> Result<(), ApiError> {
    let thread = snapshot.thread.clone();
    let snapshot = snapshot.clone();
    let group = group.to_owned();
    let call = call.to_owned();
    let index = control.index.clone();
    control.store.database.call(move|db|{let tx=db.transaction()?;let node=id();
        if let Some(execution) = result.get("execution_id").and_then(Value::as_str) {
            let (command,cwd,thread,run,turn):(Option<String>,String,String,String,String) = tx.query_row(
                "SELECT json_extract(s.shell_json,'$.command'),s.cwd,r.thread_id,s.owner_run_id,s.turn_id FROM shell_execution s JOIN run r ON r.id=s.owner_run_id WHERE s.id=?1 AND s.owner_run_id=?2 AND s.turn_id=?3 AND EXISTS(SELECT 1 FROM tool_call c WHERE c.group_id=?4 AND c.call_id=?5 AND c.operation_id=s.id)",
                params![execution,snapshot.run,snapshot.turn,group,call], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
            )?;
            if let Some(command) = command { result["command"] = json!(command); }
            result["cwd"] = json!(cwd);
            result["thread_id"] = json!(thread);
            result["run_id"] = json!(run);
            result["turn_id"] = json!(turn);
        }
        history::append_node(&tx,&snapshot.thread,&node,Some(&snapshot.turn),"tool_result",&json!({"item":{"type":"function_call_output","call_id":call,"output":result.to_string()},"group_id":group}),&now())?;
        tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES(?1,?2,?3,?4)",params![group,call,node,result.get("status").and_then(Value::as_str).unwrap_or("completed")])?;
        index.authorize_new_tx(&tx,std::slice::from_ref(&node)).map_err(crate::storage::StorageError::from)?;
        super::state::complete_tool_call(&tx, &group, &call)?;
        if matches!(result.get("status").and_then(Value::as_str),Some("exited"|"failed"|"stopped"))
            && let Some(execution)=result.get("execution_id").and_then(Value::as_str)
        {
            tx.execute("UPDATE delivery SET state='processed' WHERE event_id=?1 AND recipient_id=?2",params![format!("shell-ended-{execution}"),snapshot.run])?;
        }
        tx.commit()?;Ok(())}).await.map_err(storage_error)?;
    // Shell collection can notify before this canonical result exists. Publish
    // its own committed history change even when the next HTTP sends no bytes.
    control.changed(&thread);
    Ok(())
}

async fn run_shell(
    control: Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    group: &str,
    call_id: &str,
    args: ShellToolArgs,
) -> Result<(Value, bool), ApiError> {
    let turn_binding = slot.bind_turn(&control, &snapshot.turn).await?;
    if turn_binding.cancel.is_cancelled() && !slot.stop.is_cancelled() {
        return Ok((
            json!({"status":"cancelled","executed":false,"reason":"human_turn_interrupted","message":"A human interrupted this Turn before this call started"}),
            true,
        ));
    }
    if control.shell.starts_closed() {
        return Ok((
            json!({"status":"cancelled","reason":"owner_shutdown_before_spawn"}),
            true,
        ));
    }
    control.gc.require_capacity().await?;
    let execution = id();
    let current = snapshot.clone();
    let execution_id = execution.clone();
    let group = group.to_owned();
    let call_id = call_id.to_owned();
    let shell_config = json!({"command":args.command,"description":args.description,"kind":control.store.config.shell.kind});
    let args_copy = args.clone();
    let ready=control.store.database.call(move|db|{let tx=db.transaction()?;
        let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id WHERE u.id=?1 AND u.run_id=?2 AND u.generation=?3 AND u.interrupt_requested_at IS NULL AND u.state IN ('preparing','running','waiting') AND r.state='live')",params![current.turn,current.run,current.generation],|r|r.get(0))?;if !live{return Ok(false);}
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,?3,?1,'starting',?4,?5,?6,?7,?8,?9)",params![execution_id,current.run,current.turn,current.cwd.to_string_lossy(),shell_config.to_string(),args_copy.input_encoding,args_copy.output_encoding,if args_copy.stdin_open{"open"}else{"closed"},now()])?;
        super::state::start_tool_call(&tx, &group, &call_id, &execution_id)?;
        tx.commit()?;Ok(true)}).await.map_err(storage_error)?;
    if !ready {
        slot.bind_turn(&control, &snapshot.turn).await?;
    }
    if !ready || slot.stop.is_cancelled() {
        if turn_binding.cancel.is_cancelled() && !slot.stop.is_cancelled() {
            return Ok((
                json!({"status":"cancelled","executed":false,"reason":"human_turn_interrupted","message":"A human interrupted this Turn before this call started"}),
                true,
            ));
        }
        return Err(invalid("Run stopped before shell spawn"));
    }
    if control.shell.starts_closed() {
        return cancel_unstarted_shell(&control, &execution).await;
    }
    let origin = Origin {
        instance_id: control.store.instance_id.clone(),
        thread_id: snapshot.thread.clone(),
        run_id: snapshot.run.clone(),
        turn_id: snapshot.turn.clone(),
        execution_id: execution.clone(),
        token: id(),
    };
    control
        .origins
        .lock()
        .await
        .insert(origin.token.clone(), origin.clone());
    let mut env = shell_output::cli_environment(&control, &origin.thread_id);
    for (name, value) in [
        ("SHELLBOY_INSTANCE_ID", origin.instance_id.clone()),
        ("SHELLBOY_RUN_ID", origin.run_id.clone()),
        ("SHELLBOY_TURN_ID", origin.turn_id.clone()),
        ("SHELLBOY_EXECUTION_ID", origin.execution_id.clone()),
        ("SHELLBOY_ORIGIN_TOKEN", origin.token.clone()),
    ] {
        env.insert(name.into(), value);
    }
    let (stop, _) = match control.shell_start_intent(&execution, false, None).await {
        Ok(intent) => intent,
        Err(error) => {
            control.origins.lock().await.remove(&origin.token);
            return Err(error);
        }
    };
    if control.shell.starts_closed() {
        control.origins.lock().await.remove(&origin.token);
        return cancel_unstarted_shell(&control, &execution).await;
    }
    if stop || slot.stop.is_cancelled() {
        control.origins.lock().await.remove(&origin.token);
        return Ok((
            json!({"status":"stopped","execution_id":execution,"reason":"stopped_before_spawn"}),
            true,
        ));
    }
    let spawned = match control
        .shell
        .spawn_guarded(
            execution.clone(),
            snapshot.cwd.clone(),
            args.clone(),
            env,
            turn_binding.start_gate.clone(),
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            if error
                .details
                .as_ref()
                .is_some_and(|details| details["reason"] == "turn_interrupted_before_spawn")
            {
                control.origins.lock().await.remove(&origin.token);
                let execution_copy = execution.clone();
                let wait_wakes = control.waits.keyed.clone();
                control
                    .store
                    .database
                    .call(move |db| {
                        if super::state::cancel_shell_spawn(db, &execution_copy, &now())? != 0 {
                            wait_wakes.publish(Key::shell_terminal(execution_copy));
                        }
                        Ok(())
                    })
                    .await
                    .map_err(storage_error)?;
                return Ok((
                    json!({"status":"cancelled","executed":false,"execution_id":execution,"reason":"human_turn_interrupted","message":"A human interrupted this Turn before native process creation"}),
                    true,
                ));
            }
            if error
                .details
                .as_ref()
                .is_some_and(|details| details["reason"] == "shutdown_before_spawn")
            {
                control.origins.lock().await.remove(&origin.token);
                return cancel_unstarted_shell(&control, &execution).await;
            }
            let execution = execution.clone();
            let saved_error = error.clone();
            control.origins.lock().await.remove(&origin.token);
            let wait_wakes = control.waits.keyed.clone();
            control
                .store
                .database
                .call(move |db| {
                    if super::state::fail_shell_spawn(db, &execution, &now(), &saved_error)? != 0 {
                        wait_wakes.publish(Key::shell_terminal(execution));
                    }
                    Ok(())
                })
                .await
                .map_err(storage_error)?;
            return Err(error);
        }
    };
    let handle = spawned.handle.clone();
    let (done_receive, release) = shell_output::install_shell(
        &control,
        shell_output::ShellOwner::Run {
            thread: snapshot.thread.clone(),
            run: snapshot.run.clone(),
            turn: snapshot.turn.clone(),
        },
        Some((slot, origin.token, args.background)),
        &execution,
        spawned,
    )
    .await?;
    let mut done_receive = done_receive.expect("Run shell has a completion observer");
    if args.background {
        return Ok((
            json!({"status":"running","execution_id":execution,"background":true}),
            false,
        ));
    }
    let mut release = release.expect("foreground call registered before running publication");
    control.changed.notify_waiters();
    let released = |reason| {
        Ok((
            json!({"status":"running","execution_id":execution,"reason":reason,"cancel_remaining_calls":true}),
            true,
        ))
    };
    let result = async {
            if turn_binding.cancel.is_cancelled() && !slot.stop.is_cancelled() { return released("human_turn_interrupted"); }
            tokio::select! {
                result=&mut done_receive=>{slot.foreground.clear();let completion=result.map_err(|_|invalid("Shell collector exited"))??;let stop_calls=completion.exit_code!=Some(0)||completion.state!=ShellState::Exited||!completion.output_complete;let result=shell_result(&control,&execution,completion).await?;let stop_calls=stop_calls||result.get("wait_release_outcome").is_some_and(|value|!value.is_null());Ok((result,stop_calls))},
                _=slot.stop.cancelled()=>{handle.stop();Ok((json!({"status":"stopping","execution_id":execution,"reason":"run_stopped"}),true))},
                _=turn_binding.cancel.cancelled()=>{if slot.stop.is_cancelled(){handle.stop();return Ok((json!({"status":"stopping","execution_id":execution,"reason":"run_stopped"}),true));}released("human_turn_interrupted")},
                reason=release.recv()=> match reason {
                    Some(ForegroundRelease::Background) => released("shell_backgrounded"),
                    None => Err(invalid("Foreground release channel closed")),
                },
            }
    }.await;
    slot.foreground.clear();
    let current = snapshot.clone();
    let resumed = control.store.database.call(move |db| {
        Ok(db.execute("UPDATE turn SET state='running' WHERE id=?1 AND generation=?2 AND state='waiting' AND EXISTS(SELECT 1 FROM run WHERE id=?3 AND state='live')", params![current.turn,current.generation,current.run])?)
    }).await.map_err(storage_error)?;
    if resumed > 0 {
        control.changed(&snapshot.thread);
    }
    result
}

async fn cancel_unstarted_shell(
    control: &Control,
    execution: &str,
) -> Result<(Value, bool), ApiError> {
    let id = execution.to_owned();
    let wait_wakes = control.waits.keyed.clone();
    control
        .store
        .database
        .call(move |db| {
            if super::state::cancel_shell_spawn(db, &id, &now())? != 0 {
                wait_wakes.publish(Key::shell_terminal(id));
            }
            Ok(())
        })
        .await
        .map_err(storage_error)?;
    Ok((
        json!({"status":"cancelled","execution_id":execution,"reason":"owner_shutdown_before_spawn"}),
        true,
    ))
}

async fn shell_result(
    control: &Control,
    execution: &str,
    completion: ShellCompletion,
) -> Result<Value, ApiError> {
    let reader = super::output::ShellOutputReader::new(
        control.store.database.clone(),
        control.store.instance_id.clone(),
    );
    let text = reader
        .excerpt(
            execution,
            control.store.config.limits.tool_excerpt_bytes as usize,
        )
        .await?;
    let execution_id = execution.to_owned();
    let (wait_release,started_at,ended_at):(Option<String>,String,Option<String>)=control.store.database.call(move|db|Ok(db.query_row("SELECT json_extract(shell_json,'$.wait_release_outcome'),started_at,ended_at FROM shell_execution WHERE id=?1",[execution_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)).await.map_err(storage_error)?;
    Ok(
        json!({"status":completion.state,"execution_id":completion.execution_id,"exit_code":completion.exit_code,"forced":completion.forced,"exit_signal":completion.exit_signal,"group_cleanup_confirmed":completion.group_cleanup_confirmed,"reason":completion.reason,"output_complete":completion.output_complete,"output":text,"wait_release_outcome":wait_release,"started_at":started_at,"ended_at":ended_at,"elapsed_ms":completion.elapsed_ms}),
    )
}

#[cfg(test)]
pub(super) mod preparation_tests;

async fn spill_and_summarize(
    compactor: &crate::compaction::Compactor,
    thread: &str,
    cancel: CancellationToken,
    boundary: Option<&mut crate::compaction::boundary::Boundary<'_>>,
    request_run: Option<String>,
) -> Result<crate::compaction::CompactResult, ApiError> {
    let spill = compactor
        .spill_cancellable(thread, true, cancel.clone())
        .await?;
    let summary = compactor
        .summarize_guarded(thread, true, cancel, boundary, request_run)
        .await?;
    Ok(if summary.no_target && !spill.no_target {
        spill
    } else {
        summary
    })
}

#[cfg(test)]
mod overflow_tests;

#[cfg(test)]
mod recovery_notification_tests;

#[cfg(test)]
mod revisit_tests;

#[cfg(test)]
mod awaiting_own_ask_tests;
