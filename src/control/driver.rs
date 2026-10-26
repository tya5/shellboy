//! Per-Thread model/tool loop. Long waits never occupy operation admission.
mod adoption;
mod overflow;
mod request;
mod shell_output;
mod tools;
use adoption::adopt_response;
use overflow::OverflowBudget;
use request::{AdmittedRequest, Instructions, prepare_attempt, stream_response};
use tools::run_tool_group;

use super::state::{now, recover_turn_tx};
use super::*;
use crate::{
    history,
    model::{self, http::HttpModelClient},
    shell::ShellCompletion,
};
use futures_util::FutureExt;
use rusqlite::{OptionalExtension, params};
use shell_output::collect_shell;
use std::collections::BTreeMap;

#[derive(Clone)]
struct Snapshot {
    thread: String,
    run: String,
    turn: String,
    generation: i64,
    selection: i64,
    profile_revision: i64,
    profile: String,
    model: String,
    effort: Option<String>,
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
    Done,
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, message)
}

async fn admit_attempt(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    attempt: &str,
    attempt_attachments: Arc<crate::attachments::PreparedAttachments>,
) -> Result<bool, ApiError> {
    let current = snapshot.clone();
    let attempt_id = attempt.to_owned();
    let result = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let admitted = current.admit_attempt_tx(&tx, &attempt_id, &attempt_attachments)?;
            if !admitted {
                return Ok(false);
            }
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
    result
}

pub(super) async fn drive(
    control: Arc<Control>,
    thread: String,
    slot: DriverSlot,
) -> Result<(), ApiError> {
    let boundary = control.boundary(&thread).await;
    let mut overflow = OverflowBudget::default();
    let mut instructions = Instructions::default();
    loop {
        if slot.stop.is_cancelled() {
            return drain_and_stop(&control, &slot.run_id, Some(&thread)).await;
        }
        let _ = slot.nudge.notified().now_or_never();
        let boundary_guard = tokio::select! {biased;_=slot.stop.cancelled()=>return drain_and_stop(&control,&slot.run_id,Some(&thread)).await,guard=boundary.lock()=>guard};
        let snapshot = match next(&control, &thread, &slot.run_id).await? {
            Next::Done => return Ok(()),
            Next::Idle => {
                instructions.clear();
                drop(boundary_guard);
                tokio::select! {_=slot.stop.cancelled()=>{},_=slot.nudge.notified()=>{}};
                continue;
            }
            Next::Ready(snapshot) => snapshot,
        };
        overflow.observe(&snapshot.turn, snapshot.generation);
        if let Err(error) = control.gc.require_capacity().await {
            return fail_turn(&control, &snapshot, error).await;
        }
        let Some(mut snapshot) = instructions.refresh(&control, snapshot, &slot).await? else {
            continue;
        };
        let Some(AdmittedRequest {
            client,
            prepared,
            request,
            attempt,
        }) = prepare_attempt(&control, &snapshot, &slot, instructions.body()).await?
        else {
            continue;
        };
        if slot.stop.is_cancelled() {
            return drain_and_stop(&control, &slot.run_id, Some(&thread)).await;
        }
        let outcome =
            stream_response(&control, &thread, &attempt, &client, request, &slot.stop).await;
        // The request has ended, including failure/cancellation. Compaction or
        // shutdown can wait independently and must not retain its replay pin.
        snapshot.input.take();
        if slot.stop.is_cancelled() {
            return drain_and_stop(&control, &slot.run_id, Some(&thread)).await;
        }
        let response = match outcome {
            Ok(value) => {
                if let Some(observation) = &value.stream_observation {
                    // Fixed, bounded classifications only. Adoption owns the received audit data.
                    for difference in &observation.differences {
                        control.diagnostics.record_model_stream_difference(
                            crate::diagnostics::DiagnosticIds {
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
            Err(failure) => {
                control.diagnostics.record_model_failure(
                    crate::diagnostics::DiagnosticIds {
                        thread_id: Some(snapshot.thread.clone()),
                        run_id: Some(snapshot.run.clone()),
                        turn_id: Some(snapshot.turn.clone()),
                        execution_id: None,
                    },
                    &attempt,
                    &failure.error,
                );
                overflow::retry_or_recover(
                    &control,
                    &snapshot,
                    &slot,
                    attempt,
                    failure,
                    &mut overflow,
                )
                .await?;
                continue;
            }
        };
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
        )
        .await?;
        if !may_execute {
            continue;
        }
        drop(prepared);
        let Some(group) = group else {
            continue;
        };
        run_tool_group(&control, &snapshot, &slot, &group, response.calls).await?;
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
    let saved=control.store.database.call(move|db|Ok(db.query_row("SELECT s.body,s.observed_size FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.id=?1",[turn],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<i64>>(1)?))).optional()?)).await.map_err(storage_error)?;
    if let Some((body, size)) = saved {
        return Ok(size.map(|_| Arc::from(body)));
    }
    let prepared = control
        .instructions
        .prepare(snapshot.cwd.clone(), snapshot.agents.clone())
        .await?;
    let body = prepared.as_ref().map(|value| value.body.clone());
    let turn = snapshot.turn.clone();
    let source_path = snapshot.agents.clone();
    control.store.database.call(move|db| {
        let tx=db.transaction()?;
        let snapshot_id=id();
        let (path,hash,body,size,modified)=if let Some(value)=prepared {
            (value.source_path.to_string_lossy().into_owned(),value.hash,value.body.to_string(),Some(value.size as i64),value.modified.map(|value|format!("{value:?}")))
        } else {
            (source_path.to_string_lossy().into_owned(),"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),String::new(),None,None)
        };
        let (thread,run):(String,String)=tx.query_row("SELECT thread_id,run_id FROM turn WHERE id=?1",[&turn],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let previous:Option<(String,String,Option<i64>)>=tx.query_row("SELECT s.source_path,s.content_hash,s.observed_size FROM turn t JOIN instruction_snapshot s ON s.id=t.instruction_snapshot_id WHERE t.thread_id=?1 AND t.id<>?2 ORDER BY t.created_at DESC,t.rowid DESC LIMIT 1",params![thread,turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if previous.as_ref()!=Some(&(path.clone(),hash.clone(),size)) {
            super::context::record(&tx,&thread,"instructions_observed",json!({"kind":"shellboy","run_id":run}),json!({"before":previous.map(|(path,hash,size)|json!({"path":path,"hash":hash,"present":size.is_some()})),"after":{"path":path,"hash":hash,"present":size.is_some()}}),&now())?;
        }
        tx.execute("INSERT INTO instruction_snapshot(id,source_path,content_hash,body,observed_size,observed_mtime,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![snapshot_id,path,hash,body,size,modified,now()])?;
        tx.execute("UPDATE turn SET instruction_snapshot_id=?2 WHERE id=?1 AND instruction_snapshot_id IS NULL",params![turn,snapshot_id])?;
        tx.commit()?;
        Ok(())
    }).await.map_err(storage_error)?;
    Ok(body)
}

async fn next(control: &Arc<Control>, thread: &str, run_id: &str) -> Result<Next, ApiError> {
    let thread_id = thread.to_owned();
    let run_target = run_id.to_owned();
    let thread = thread_id.clone();
    let run_id = run_target.clone();
    let index = control.index.clone();
    let replay = control.replay.clone();
    let result = control.store.database.call(move|db|{
        let tx=db.transaction()?;
        let run:Option<String>=tx.query_row("SELECT id FROM run WHERE thread_id=?1 AND id=?2 AND state='live'",params![thread,run_id],|r|r.get(0)).optional()?;
        let Some(run)=run else{return Ok(Next::Done);};
        let mut turn:Option<String>=tx.query_row("SELECT id FROM turn WHERE run_id=?1 AND state IN ('preparing','running','waiting')",[&run],|r|r.get(0)).optional()?;
        let recovering:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turn WHERE run_id=?1 AND state='recovery_wait')",[&run],|r|r.get(0))?;
        if recovering{return Ok(Next::Done);}
        let attachment_count = super::attachment_delivery::apply_pending_tx(&tx,&thread,&now()).map_err(crate::storage::StorageError::from)?;
        let attachment_blocked=super::attachment_delivery::has_pending_tx(&tx,&thread).map_err(crate::storage::StorageError::from)?;
        // A foreign shell group can still be running. Do not let a later human
        // receipt overtake its accepted addition. Roll back partial adoption;
        // the originating group's completion nudges this driver again.
        if attachment_blocked { return Ok(Next::Idle); }
        let events:Vec<(String,String)>=tx.prepare("SELECT e.id,json_object('event_id',e.id,'kind',e.kind,'payload',json(e.payload_ref_json)) FROM event e JOIN delivery d ON d.event_id=e.id WHERE d.recipient_id=?1 AND d.state='pending' AND e.kind<>'context_changed' ORDER BY e.created_at,e.id")?.query_map([&run],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        if turn.is_none()&&(!events.is_empty()||attachment_count>0){let new_turn=id();tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'preparing',?4)",params![new_turn,thread,run,now()])?;super::context::turn_started(&tx,&thread,&run,&new_turn,&now())?;turn=Some(new_turn);}
        let Some(turn)=turn else{
            let owned:i64=tx.query_row("SELECT (SELECT count(*) FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping'))+(SELECT count(*) FROM run WHERE owner_run_id=?1 AND state IN ('live','stopping'))",[&run],|r|r.get(0))?;
            if owned>0{return Ok(Next::Idle);}
            super::state::complete_idle_run(&tx, &thread, &run)?;
            tx.commit()?;return Ok(Next::Done);
        };
        index.record_context_observations_tx(&tx,&thread,&now()).map_err(crate::storage::StorageError::from)?;
        // Context-only observations have no delivery until an actual Turn is
        // already runnable: settings never wake background/recovery waits.
        tx.execute("INSERT OR IGNORE INTO delivery(event_id,recipient_id,state) SELECT e.id,?2,'pending' FROM event e WHERE e.target_thread_id=?1 AND e.kind='context_changed' AND e.target_run_id IS NULL AND NOT EXISTS(SELECT 1 FROM delivery d WHERE d.event_id=e.id AND d.state='processed')",params![thread,run])?;
        let events:Vec<(String,String)>=tx.prepare("SELECT e.id,json_object('event_id',e.id,'kind',e.kind,'payload',json(e.payload_ref_json)) FROM event e JOIN delivery d ON d.event_id=e.id WHERE d.recipient_id=?1 AND d.state='pending' AND (e.kind<>'context_changed' OR NOT EXISTS(SELECT 1 FROM delivery done WHERE done.event_id=e.id AND done.state='processed')) ORDER BY e.created_at,e.id")?.query_map([&run],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        let mut snapshot=tx.query_row("SELECT u.generation,t.selection_revision,t.profile_revision,t.profile_id,t.model_id,t.effort,t.reasoning_epoch,t.cwd,t.agents_path FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=?1",[&turn],|r|Ok(Snapshot{thread:thread.clone(),run:run.clone(),turn:turn.clone(),generation:r.get(0)?,selection:r.get(1)?,profile_revision:r.get(2)?,profile:r.get(3)?,model:r.get(4)?,effort:r.get(5)?,epoch:r.get(6)?,cwd:PathBuf::from(r.get::<_,String>(7)?),agents:PathBuf::from(r.get::<_,String>(8)?),input:None,receipts:vec![],events:events.iter().map(|v|v.0.clone()).collect()}))?;
        let mut path:Vec<String>=tx.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?.query_map([&thread],|r|r.get(0))?.collect::<Result<_,_>>()?;
        let pending:Vec<(String,String)>=tx.prepare("SELECT id,message_node_id FROM receipt WHERE turn_id=?1 AND state='accepted' ORDER BY created_at,rowid")?.query_map([&turn],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        for (receipt,node)in pending{snapshot.receipts.push(receipt);if !path.contains(&node){path.push(node);}}
        if path.len()as i64!=tx.query_row("SELECT count(*) FROM thread_path WHERE thread_id=?1",[&thread],|r|r.get::<_,i64>(0))?{history::select_path(&tx,&thread,snapshot.selection as u64,&path,&now())?;}
        for (event,payload)in &events{
            let node=format!("event-{event}");
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM node WHERE id=?1)",[&node],|r|r.get(0))?;
            if !exists{history::append_node(&tx,&thread,&node,Some(&turn),"event",&json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {payload}")}],"event_id":event}),&now())?;}
        }
        snapshot.selection=history::selection_revision(&tx, &thread, history::ThreadScope::Any)?;
        snapshot.input=Some(replay.load(&tx,&thread)?);
        tx.commit()?;Ok(Next::Ready(Box::new(snapshot)))
    }).await.map_err(storage_error);
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
                let turn = id();
                tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'preparing',?4)",params![turn,thread,run,now()])?;
                super::context::turn_started(&tx,&thread,&run,&turn,&now())?;
                (turn,0)
            }
        };
        recover_turn_tx(&tx,&thread,&run,&turn,generation,&error)?;
        tx.commit()?;
        Ok(Some(turn))
    }).await.map_err(storage_error)?;
    if let Some(turn) = target {
        control
            .diagnostics
            .record_preparation_failure(crate::diagnostics::DiagnosticIds {
                thread_id: Some(target_thread.clone()),
                run_id: Some(target_run.clone()),
                turn_id: Some(turn),
                execution_id: None,
            });
        control.changed(&target_thread);
        control.nudge_owner(&target_run).await;
    }
    // Keep the original typed failure as the one driver error. The recovery
    // reason is persisted above; the caller must not silently report success.
    Ok(())
}

async fn recover(
    control: &Arc<Control>,
    snapshot: &Snapshot,
    error: ApiError,
    attempt: Option<(String, Value)>,
) -> Result<(), ApiError> {
    // An attempt failure was already classified at the typed ModelError boundary.
    if attempt.is_none() {
        control.diagnostics.record(
            crate::diagnostics::DiagnosticEvent::ModelFailure,
            crate::diagnostics::DiagnosticIds {
                thread_id: Some(snapshot.thread.clone()),
                run_id: Some(snapshot.run.clone()),
                turn_id: Some(snapshot.turn.clone()),
                execution_id: None,
            },
        );
    }
    let thread_id = snapshot.thread.clone();
    let run_id = snapshot.run.clone();
    let snapshot = snapshot.clone();
    control.store.database.call(move|db|{let tx=db.transaction()?;
        recover_turn_tx(&tx,&snapshot.thread,&snapshot.run,&snapshot.turn,snapshot.generation,&error)?;
        if let Some((attempt,mut body))=attempt{
            body["attempt_id"] = json!(attempt);
            super::state::incomplete_attempt(&tx, &attempt, &body, &now())?;
            tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'incomplete_response',?3,?4,?5)",params![id(),snapshot.turn,now(),body.to_string(),body.to_string().len()as i64])?;
            // Audit-only nodes are roots through their live Turn, without being
            // selected for replay. Invalidate a GC mark made before this INSERT.
            if tx.execute("UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",[])? != 1 {
                return Err(crate::storage::StorageError::Schema("root revision metadata missing".into()));
            }
        }
        tx.commit()?;Ok(())}).await.map_err(storage_error)?;
    control.changed(&thread_id);
    control.nudge_owner(&run_id).await;
    Ok(())
}
pub(super) async fn stop_without_driver(control: &Control, run_id: &str) -> Result<(), ApiError> {
    drain_and_stop(control, run_id, None).await
}

/// Freeze ownership first; cancel processes and child drivers; record terminal
/// state only after every owned resource has actually released its live handle.
async fn drain_and_stop(
    control: &Control,
    run_id: &str,
    current_thread: Option<&str>,
) -> Result<(), ApiError> {
    let run_id = run_id.to_owned();
    let owned = control.store.database.call(move |db| {
        let tx = db.transaction()?;
        let owned: Vec<(String,String)> = tx.prepare("WITH RECURSIVE owned(id,thread_id,depth) AS(SELECT id,thread_id,0 FROM run WHERE id=?1 UNION ALL SELECT r.id,r.thread_id,o.depth+1 FROM run r JOIN owned o ON r.owner_run_id=o.id WHERE r.state IN ('live','stopping')) SELECT id,thread_id FROM owned ORDER BY depth DESC,id")?.query_map([run_id],|row|Ok((row.get(0)?,row.get(1)?)))?.collect::<Result<_,_>>()?;
        for (run,_) in &owned {
            super::attachment_delivery::hold_run_tx(&tx,run).map_err(crate::storage::StorageError::from)?;
            tx.execute("UPDATE run SET state='stopping' WHERE id=?1 AND state='live'",[run])?;
            tx.execute("UPDATE turn SET generation=generation+1 WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",[run])?;
        }
        tx.commit()?;
        Ok(owned)
    }).await.map_err(storage_error)?;
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
    control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            super::state::stop_drained_runs(&tx, &owned, stopping_for_shutdown)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(storage_error)
}

async fn save_tool_result(
    control: &Control,
    snapshot: &Snapshot,
    group: &str,
    call: &str,
    mut result: Value,
) -> Result<(), ApiError> {
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
        tx.commit()?;Ok(())}).await.map_err(storage_error)
}

async fn run_shell(
    control: Arc<Control>,
    snapshot: &Snapshot,
    slot: &DriverSlot,
    group: &str,
    call_id: &str,
    args: ShellToolArgs,
) -> Result<(Value, bool), ApiError> {
    control.gc.require_capacity().await?;
    let execution = id();
    let current = snapshot.clone();
    let execution_id = execution.clone();
    let group = group.to_owned();
    let call_id = call_id.to_owned();
    let shell_config = json!({"command":args.command,"kind":control.store.config.shell.kind});
    let args_copy = args.clone();
    let ready=control.store.database.call(move|db|{let tx=db.transaction()?;
        let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND state='live')",[&current.run],|r|r.get(0))?;if !live{return Ok(false);}
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,?3,?1,'starting',?4,?5,?6,?7,?8,?9)",params![execution_id,current.run,current.turn,current.cwd.to_string_lossy(),shell_config.to_string(),args_copy.input_encoding,args_copy.output_encoding,if args_copy.stdin_open{"open"}else{"closed"},now()])?;
        super::state::start_tool_call(&tx, &group, &call_id, &execution_id)?;
        tx.commit()?;Ok(true)}).await.map_err(storage_error)?;
    if !ready || slot.stop.is_cancelled() {
        return Err(invalid("Run stopped before shell spawn"));
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
    let mut env = BTreeMap::new();
    for (name, value) in [
        (
            "SHELLBOY_DATA_DIR",
            control.data_dir.to_string_lossy().into_owned(),
        ),
        ("SHELLBOY_INSTANCE_ID", origin.instance_id.clone()),
        ("SHELLBOY_THREAD_ID", origin.thread_id.clone()),
        ("SHELLBOY_RUN_ID", origin.run_id.clone()),
        ("SHELLBOY_TURN_ID", origin.turn_id.clone()),
        ("SHELLBOY_EXECUTION_ID", origin.execution_id.clone()),
        ("SHELLBOY_ORIGIN_TOKEN", origin.token.clone()),
    ] {
        env.insert(name.into(), value);
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        let mut paths = vec![parent.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        if let Ok(path) = std::env::join_paths(paths) {
            env.insert("PATH".into(), path.to_string_lossy().into_owned());
        }
    }
    let (stop, _) = match control.shell_start_intent(&execution, false).await {
        Ok(intent) => intent,
        Err(error) => {
            control.origins.lock().await.remove(&origin.token);
            return Err(error);
        }
    };
    if stop || slot.stop.is_cancelled() {
        control.origins.lock().await.remove(&origin.token);
        return Ok((
            json!({"status":"stopped","execution_id":execution,"reason":"stopped_before_spawn"}),
            true,
        ));
    }
    let spawned = match control
        .shell
        .spawn(execution.clone(), snapshot.cwd.clone(), args.clone(), env)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            let execution = execution.clone();
            control.origins.lock().await.remove(&origin.token);
            control
                .store
                .database
                .call(move |db| {
                    super::state::fail_shell_spawn(db, &execution, &now())?;
                    Ok(())
                })
                .await
                .map_err(storage_error)?;
            return Err(error);
        }
    };
    let handle = spawned.handle.clone();
    control.live.shells.lock().await.insert(
        execution.clone(),
        LiveShell {
            run_id: snapshot.run.clone(),
            handle: handle.clone(),
        },
    );
    let collector = control.clone();
    let current = snapshot.clone();
    let execution_copy = execution.clone();
    let nudge = slot.nudge.clone();
    let (done_send, mut done_receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = collect_shell(&collector, &current, &execution_copy, spawned).await;
        collector.live.shells.lock().await.remove(&execution_copy);
        collector.origins.lock().await.remove(&origin.token);
        if let Err(error) = &result {
            if error.is_storage_failure() {
                collector.fail_storage().await;
            }
            let _ = collector.events.send(EngineEvent::Error {
                thread_id: current.thread.clone(),
                error: error.clone(),
            });
        }
        // Foreground completion uses its direct result. Only an absent observer
        // needs a separate background completion wakeup.
        let unobserved = done_send.send(result).is_err();
        if unobserved {
            nudge.notify_one();
        }
        collector.changed(&current.thread);
    });
    // The collector owns the process before any further fallible DB write. A
    // failed running-state save cannot strand an unobserved OS execution.
    if let Err(error) = control.reapply_shell_intent(&execution, &handle).await {
        handle.stop();
        if error.is_storage_failure() {
            control.fail_storage().await;
        }
        return Err(error);
    }
    control.changed(&snapshot.thread);
    if args.background {
        return Ok((
            json!({"status":"running","execution_id":execution,"background":true}),
            false,
        ));
    }
    let mut release = slot.foreground.register(&execution);
    control.changed.notify_waiters();
    let released = |reason| {
        Ok((
            json!({"status":"running","execution_id":execution,"reason":reason,"cancel_remaining_calls":true}),
            true,
        ))
    };
    // Register before inspecting durable input. A send during the model request
    // or spawn remains accepted even if no foreground channel existed then.
    // Recheck after every interrupt signal: a late kick for already-applied
    // input must not release a later call, and later real input must still wake.
    let result = async {
        loop {
            let turn = snapshot.turn.clone();
            let pending = control.store.database.call(move |db| {
                Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM receipt WHERE turn_id=?1 AND state='accepted')", [turn], |row| row.get::<_,bool>(0))?)
            }).await.map_err(storage_error)?;
            if pending { return released("additional_input"); }
            tokio::select! {
                result=&mut done_receive=>{slot.foreground.clear();let completion=result.map_err(|_|invalid("Shell collector exited"))??;let stop_calls=completion.exit_code!=Some(0)||completion.state!=ShellState::Exited||!completion.output_complete;let result=shell_result(&control,&execution,completion).await?;let stop_calls=stop_calls||result.get("wait_release_outcome").is_some_and(|value|!value.is_null());return Ok((result,stop_calls));},
                _=slot.stop.cancelled()=>{handle.stop();return Ok((json!({"status":"stopping","execution_id":execution,"reason":"run_stopped"}),true));},
                reason=release.recv()=> match reason {
                    Some(ForegroundRelease::Background) => return released("human_backgrounded"),
                    Some(ForegroundRelease::AdditionalInput) => {},
                    None => return Err(invalid("Foreground release channel closed")),
                },
            }
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
mod preparation_tests;

async fn spill_and_summarize(
    compactor: &crate::compaction::Compactor,
    thread: &str,
    cancel: CancellationToken,
) -> Result<crate::compaction::CompactResult, ApiError> {
    let spill = compactor
        .spill_cancellable(thread, true, cancel.clone())
        .await?;
    let summary = compactor.summarize(thread, true, cancel).await?;
    Ok(if summary.no_target && !spill.no_target {
        spill
    } else {
        summary
    })
}

#[cfg(test)]
mod overflow_tests;
