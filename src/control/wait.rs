//! Bounded-duration explicit observations without resource dependency policy.
use super::{Control, foreground::ForegroundSlot};
use crate::protocol::*;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct WaitKeys<'a>(&'a str);
impl<'a> WaitKeys<'a> {
    fn new(operation: &'a str) -> Self {
        Self(operation)
    }
    fn pending(self) -> String {
        format!("wait.pending.{}", self.0)
    }
}
#[cfg(test)]
mod metadata_keys_tests {
    use super::WaitKeys;
    #[test]
    fn pending_wait_key_keeps_owner_and_operation_bytes() {
        assert_eq!(
            WaitKeys::new("owner:operation:日本").pending(),
            "wait.pending.owner:operation:日本"
        );
        assert_eq!(WaitKeys::new("").pending(), "wait.pending.");
    }
}

type Result<T> = std::result::Result<T, ApiError>;
const WAIT_BINDING_BYTES: usize = r#","turn_wait_generation":9223372036854775807"#.len();
#[derive(Default)]
pub(super) struct WaitRegistry {
    #[cfg(test)]
    pub(super) observations: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(super) output_commits: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(super) output_save_micros: StdMutex<Vec<u128>>,
    entries: Arc<StdMutex<HashMap<String, Registration>>>,
}
#[derive(Clone)]
struct Registration {
    source: String,
}
struct Guard {
    entries: Arc<StdMutex<HashMap<String, Registration>>>,
    id: String,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.entries
            .lock()
            .expect("wait registry poisoned")
            .remove(&self.id);
    }
}

impl Control {
    /// Bounded in-process ownership witnesses; the frontend joins these exact
    /// operations to their committed pending records in its normal read snapshot.
    pub(crate) fn pending_wait_sources(&self) -> Option<Vec<(String, String)>> {
        let entries = self.waits.entries.lock().expect("wait registry poisoned");
        if entries.len() > 128 {
            return None;
        }
        let mut sources = entries
            .iter()
            .filter_map(|(operation, registration)| {
                registration
                    .source
                    .strip_prefix("shell:")
                    .map(|execution| (operation.clone(), execution.to_owned()))
            })
            .collect::<Vec<_>>();
        sources.sort_unstable();
        Some(sources)
    }
}
struct Observation {
    matched: Vec<WaitCondition>,
    snapshot: Value,
    recovery: bool,
    unavailable: bool,
    interrupted: bool,
    cursor: Option<String>,
}

pub(super) async fn observe(
    control: Arc<Control>,
    operation_id: String,
    args: WaitArgs,
    caller: Option<Origin>,
    disconnected: CancellationToken,
) -> Result<Value> {
    let foreground = if let Some(origin) = &caller {
        control
            .live
            .foreground_for_run(&origin.thread_id, &origin.run_id)
            .await
    } else {
        None
    };
    let cleanup_id = operation_id.clone();
    let result = observe_inner(
        control.clone(),
        operation_id,
        args,
        caller.clone(),
        foreground.clone(),
        disconnected,
    )
    .await;
    let cleanup_caller = caller.clone();
    let changed = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let key = WaitKeys::new(&cleanup_id).pending();
            let generation: Option<i64> = tx.query_row("SELECT json_extract(value,'$.turn_wait_generation') FROM app_meta WHERE key=?1",[&key],|r|r.get(0)).optional()?.flatten();
            let removed = tx.execute("DELETE FROM app_meta WHERE key=?1", [&key])?;
            let changed = if let (Some(origin),Some(foreground),Some(generation)) = (cleanup_caller,foreground,generation) {
                foreground.with_current(&origin.execution_id, || {
                    tx.execute("UPDATE turn SET state='running' WHERE id=?1 AND generation=?2 AND state='waiting' AND EXISTS(SELECT 1 FROM run WHERE id=?3 AND state='live') AND NOT EXISTS(SELECT 1 FROM app_meta WHERE key>='wait.pending.' AND key<'wait.pending/' AND json_extract(value,'$.caller.turn_id')=?1 AND json_extract(value,'$.caller.execution_id')=?4 AND json_extract(value,'$.turn_wait_generation')=?2)",rusqlite::params![origin.turn_id,generation,origin.run_id,origin.execution_id])
                }).unwrap_or(Ok(0))?
            } else { 0 };
            tx.commit()?;
            Ok(removed > 0 || changed > 0)
        })
        .await
        .map_err(crate::app::storage_error)?;
    if changed && let Some(origin) = caller {
        control.changed(&origin.thread_id);
    }
    result
}

async fn observe_inner(
    control: Arc<Control>,
    operation_id: String,
    args: WaitArgs,
    caller: Option<Origin>,
    foreground: Option<Arc<ForegroundSlot>>,
    disconnected: CancellationToken,
) -> Result<Value> {
    if args.conditions.is_empty() {
        return Err(invalid("wait requires at least one condition"));
    }
    crate::thread_observation::validate(&args.conditions, args.after.as_deref())?;
    let mut thread_after = args.after.clone();
    let duration = Duration::from_millis(args.timeout_ms);
    let started = tokio::time::Instant::now();
    let mut guard = None;
    let mut initial = true;
    let observes_output = args
        .conditions
        .iter()
        .any(|condition| matches!(condition, WaitCondition::ShellOutput { .. }));
    loop {
        let output_changed = control.shell_output_changed.notified();
        tokio::pin!(output_changed);
        if observes_output {
            output_changed.as_mut().enable();
        }
        let notified = control.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let mut query = args.clone();
        query.after = thread_after.clone();
        let origin = caller.clone();
        let durable_id = operation_id.clone();
        #[cfg(test)]
        control
            .waits
            .observations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let observation = control
            .store
            .database
            .call(move |db| {
                let tx=db.transaction()?;
                let observation=inspect(&tx,&query,origin.as_ref(),initial);

                if initial&&observation.as_ref().is_ok_and(|observation|outcome(&query,observation,None).is_ok_and(|value|value.is_none())){
                    let caller=origin.as_ref().map(|origin|json!({"thread_id":origin.thread_id,"run_id":origin.run_id,"turn_id":origin.turn_id,"execution_id":origin.execution_id}));
                    let body=json!({"operation_id":durable_id,"caller":caller,"conditions":query.conditions,"timeout_ms":query.timeout_ms,"after_cursor":observation.as_ref().ok().and_then(|o|o.cursor.as_ref()),"accepted_at":chrono::Utc::now().to_rfc3339()}).to_string();
                    if body.len().saturating_add(WAIT_BINDING_BYTES)>MAX_FRAME_BYTES{return Ok(Err(ApiError::new(ErrorCode::InputLimit,"Wait observation metadata exceeds the public frame bound")));}
                    tx.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2)",rusqlite::params![WaitKeys::new(&durable_id).pending(),body])?;
                }
                tx.commit()?;
                Ok(observation)
            })
            .await
            .map_err(crate::app::storage_error)?;
        let observation = observation?;
        if observation.cursor.is_some() {
            thread_after = observation.cursor.clone();
        }
        if let Some(result) = outcome(&args, &observation, None)? {
            return Ok(result);
        }
        if (guard.is_none()
            || args
                .conditions
                .iter()
                .any(|condition| matches!(condition, WaitCondition::ReplyReceived { .. })))
            && let Some(caller) = &caller
        {
            let mut entries = control
                .waits
                .entries
                .lock()
                .expect("wait registry poisoned");
            let registration = Registration {
                source: format!("shell:{}", caller.execution_id),
            };
            entries.insert(operation_id.clone(), registration);
            if guard.is_none() {
                guard = Some(Guard {
                    entries: control.waits.entries.clone(),
                    id: operation_id.clone(),
                });
            }
        }
        if initial
            && guard.is_some()
            && let Some(origin) = &caller
        {
            // The initial pending record committed before installing the first
            // registration. Background observers have no Turn state transition
            // that could otherwise make this newly owned wait visible.
            control.changed(&origin.thread_id);
        }
        initial = false;
        if disconnected.is_cancelled() {
            return outcome(&args, &observation, Some(WaitStatus::Interrupted))?
                .ok_or_else(|| invalid("wait cancellation did not produce a result"));
        }
        if started.elapsed() >= duration {
            return outcome(&args, &observation, Some(WaitStatus::Timeout))?
                .ok_or_else(|| invalid("wait deadline did not produce a result"));
        }
        if let (Some(origin), Some(foreground)) = (&caller, &foreground) {
            let thread = origin.thread_id.clone();
            let origin = origin.clone();
            let foreground = foreground.clone();
            let durable_id = operation_id.clone();
            let changed = control.store.database.call(move |db| {
                Ok(foreground.with_current(&origin.execution_id, || {
                    let tx = db.transaction()?;
                    let generation: Option<i64> = tx.query_row(
                        "SELECT u.generation FROM turn u JOIN run r ON r.id=u.run_id JOIN shell_execution s ON s.turn_id=u.id AND s.owner_run_id=r.id JOIN tool_call c ON c.operation_id=s.id JOIN tool_group g ON g.id=c.group_id JOIN model_attempt a ON a.id=g.attempt_id WHERE u.id=?1 AND r.id=?2 AND r.thread_id=?3 AND s.id=?4 AND u.state IN ('running','waiting') AND r.state='live' AND s.state IN ('starting','running') AND c.state='running' AND g.state='running' AND a.generation=u.generation",
                        rusqlite::params![origin.turn_id,origin.run_id,origin.thread_id,origin.execution_id], |row| row.get(0),
                    ).optional()?;
                    let changed = if let Some(generation) = generation {
                        tx.execute("UPDATE app_meta SET value=json_set(value,'$.turn_wait_generation',?2) WHERE key=?1 AND json_extract(value,'$.turn_wait_generation') IS NOT ?2",rusqlite::params![WaitKeys::new(&durable_id).pending(),generation])?;
                        tx.execute("UPDATE turn SET state='waiting' WHERE id=?1 AND generation=?2 AND state='running'",rusqlite::params![origin.turn_id,generation])?
                    } else { 0 };
                    tx.commit()?;
                    Ok::<_,rusqlite::Error>(changed > 0)
                }).unwrap_or(Ok(false))?)
            }).await.map_err(crate::app::storage_error)?;
            if changed {
                control.changed(&thread);
            }
        }
        tokio::select! {
            _=notified=>{},
            _=output_changed, if observes_output=>{},
            _=disconnected.cancelled()=>{},
            _=tokio::time::sleep(duration.saturating_sub(started.elapsed()).min(Duration::from_secs(86_400)))=>{},
            _=control.stop.cancelled()=>return outcome(&args,&observation,Some(WaitStatus::Interrupted))?.ok_or_else(||invalid("shutdown wait result missing")),
        }
    }
}
fn outcome(
    args: &WaitArgs,
    observation: &Observation,
    fallback: Option<WaitStatus>,
) -> Result<Option<Value>> {
    let status = if !observation.matched.is_empty() {
        Some(WaitStatus::Satisfied)
    } else if observation.unavailable {
        Some(WaitStatus::Unavailable)
    } else if observation.recovery {
        Some(WaitStatus::RecoveryWait)
    } else if observation.interrupted {
        Some(WaitStatus::Interrupted)
    } else {
        fallback
    };
    let Some(status) = status else {
        return Ok(None);
    };
    let cursor = observation.cursor.clone().or_else(|| {
        observation.matched.iter().find_map(|condition| {
            if let WaitCondition::ShellOutput { cursor, .. } = condition {
                Some(cursor.clone())
            } else {
                None
            }
        })
    });
    let reason = match status {
        WaitStatus::Timeout => Some(format!(
            "wait reached its {} ms deadline; targets continue",
            args.timeout_ms
        )),
        WaitStatus::Interrupted => Some("observation interrupted; targets continue".into()),
        WaitStatus::RecoveryWait => Some("target requires an explicit recovery decision".into()),
        WaitStatus::Unavailable => Some("target Thread was deleted or became unavailable".into()),
        _ => None,
    };
    serde_json::to_value(WaitResult {
        status,
        matched: observation.matched.clone(),
        snapshot: observation.snapshot.clone(),
        cursor,
        reason,
        hint: None,
    })
    .map(Some)
    .map_err(|_| invalid("cannot encode wait snapshot"))
}

fn inspect(
    db: &Connection,
    args: &WaitArgs,
    caller: Option<&Origin>,
    initial: bool,
) -> Result<Observation> {
    let mut observation = Observation {
        matched: vec![],
        snapshot: Value::Null,
        recovery: false,
        unavailable: false,
        interrupted: false,
        cursor: None,
    };
    let thread_report =
        crate::thread_observation::observe(db, &args.conditions, args.after.as_deref(), initial)?;
    let mut snapshots = Vec::new();
    for condition in &args.conditions {
        match condition {
            WaitCondition::ReplyReceived { message_id } => {
                let row = db
                    .query_row(
                        "SELECT r.result_json FROM receipt r WHERE r.message_node_id=?1",
                        [message_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()
                    .map_err(sql_error)?;
                if let Some(saved) = row {
                    let saved: Value = saved
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(Value::Null);
                    if saved["expect_reply"] != true {
                        return Err(invalid("message does not expect a reply"));
                    }
                    let reply = &saved["reply"];
                    let state = reply["state"].as_str().unwrap_or("pending");
                    snapshots.push(json!({"type":"reply","message_id":message_id,"state":state,"reply_node_id":reply["node_id"],"at":reply["at"],"by":reply["by"]}));
                    if state != "pending" {
                        observation.matched.push(condition.clone());
                    } else {
                    }
                } else {
                    missing(&mut observation, initial, "Message", message_id)?;
                }
            }
            WaitCondition::ThreadSettled { thread_ids, states } => {
                let report = thread_report.as_ref().expect("Thread condition projection");
                let changes: Vec<_> = report
                    .changes
                    .iter()
                    .filter(|change| {
                        thread_ids.iter().any(|id| change["thread_id"] == *id)
                            && states.iter().any(|state| change["state"] == *state)
                    })
                    .cloned()
                    .collect();
                if !changes.is_empty() {
                    observation.matched.push(condition.clone());
                }
                let mut snapshot = json!({"type":"thread_settled","thread_ids":thread_ids,"states":states,"changes":changes});
                let shells = owned_live_shells(db, thread_ids)?;
                if !shells.is_empty() {
                    snapshot["owned_shells"] = json!(shells);
                }
                snapshots.push(snapshot);
            }
            WaitCondition::TurnCompleted { turn_id } => {
                let row=db.query_row("SELECT u.state,u.run_id,u.answer_node_id,t.deleted_at FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=?1",[turn_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<String>>(3)?))).optional().map_err(sql_error)?;
                if let Some((state, run, answer, deleted)) = row {
                    snapshots.push(json!({"type":"turn","turn_id":turn_id,"run_id":run,"state":state,"end_message_node_id":answer,"unavailable":deleted.is_some()}));
                    if deleted.is_some() {
                        observation.unavailable = true;
                    } else if terminal(&state) {
                        observation.matched.push(condition.clone());
                    } else if state == "recovery_wait" {
                        observation.recovery = true;
                    }
                } else {
                    missing(&mut observation, initial, "Turn", turn_id)?;
                }
            }
            WaitCondition::RunEnded { run_id } => {
                let row=db.query_row("SELECT r.state,t.deleted_at,EXISTS(SELECT 1 FROM turn WHERE run_id=r.id AND state='recovery_wait') FROM run r JOIN thread t ON t.id=r.thread_id WHERE r.id=?1",[run_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,bool>(2)?))).optional().map_err(sql_error)?;
                if let Some((state, deleted, recovery)) = row {
                    snapshots.push(json!({"type":"run","run_id":run_id,"state":state,"recovery_wait":recovery,"unavailable":deleted.is_some()}));
                    if deleted.is_some() {
                        observation.unavailable = true;
                    } else if terminal(&state) {
                        observation.matched.push(condition.clone());
                    } else if recovery {
                        observation.recovery = true;
                    }
                } else {
                    missing(&mut observation, initial, "Run", run_id)?;
                }
            }
            WaitCondition::ShellEnded { execution_id }
            | WaitCondition::ShellOutput { execution_id, .. } => {
                let row=db.query_row("SELECT s.state,s.owner_run_id,s.exit_code,s.output_complete,t.deleted_at FROM shell_execution s JOIN run r ON r.id=s.owner_run_id JOIN thread t ON t.id=r.thread_id WHERE s.id=?1",[execution_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<i32>>(2)?,row.get::<_,Option<bool>>(3)?,row.get::<_,Option<String>>(4)?))).optional().map_err(sql_error)?;
                if let Some((state, run, exit, complete, deleted)) = row {
                    snapshots.push(json!({"type":"shell","execution_id":execution_id,"run_id":run,"state":state,"exit_code":exit,"output_complete":complete,"unavailable":deleted.is_some()}));
                    if deleted.is_some() {
                        observation.unavailable = true;
                    } else {
                        let ready = if let WaitCondition::ShellOutput { cursor, .. } = condition {
                            super::output::wait_position(db, execution_id, cursor)?
                        } else {
                            terminal(&state)
                        };
                        if ready {
                            observation.matched.push(condition.clone());
                        }
                    }
                } else {
                    missing(&mut observation, initial, "shell", execution_id)?;
                }
            }
        }
    }
    if let Some(caller) = caller {
        observation.interrupted=db.query_row("SELECT EXISTS(SELECT 1 FROM receipt WHERE target_thread_id=?1 AND state='accepted')",[&caller.thread_id],|row|row.get(0)).map_err(sql_error)?;
    }
    observation.snapshot = json!({"conditions":snapshots});
    if let Some(report) = thread_report {
        observation.snapshot["changes"] = json!(report.changes);
        observation.snapshot["more"] = json!(report.more);
        observation.unavailable |= report.unavailable;
        observation.snapshot["thread_unavailable"] = json!(report.unavailable);
        observation.cursor = Some(report.cursor);
    }
    Ok(observation)
}
/// Live shells that keep a watched Thread's Run from completing: saved
/// execution ID and elapsed time only, bounded, so a parent that is still
/// waiting can see why without a separate query.
fn owned_live_shells(db: &Connection, thread_ids: &[String]) -> Result<Vec<Value>> {
    const LIMIT: usize = 16;
    let observed = chrono::Utc::now();
    let mut shells = Vec::new();
    let mut statement = db
        .prepare("SELECT s.id,r.id FROM run r JOIN shell_execution s ON s.owner_run_id=r.id WHERE r.thread_id=?1 AND r.state IN ('live','stopping') AND s.state IN ('starting','running','stopping') ORDER BY s.started_at,s.id LIMIT ?2")
        .map_err(sql_error)?;
    for thread in thread_ids {
        let rows = statement
            .query_map(
                rusqlite::params![thread, (LIMIT - shells.len()) as i64],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?;
        for (execution, run) in rows {
            let elapsed = crate::shell::timing::read(db, &execution, observed)
                .map_err(sql_error)?
                .elapsed_ms;
            shells.push(json!({"thread_id":thread,"run_id":run,"execution_id":execution,"elapsed_ms":elapsed}));
        }
        if shells.len() >= LIMIT {
            break;
        }
    }
    Ok(shells)
}
fn missing(observation: &mut Observation, initial: bool, kind: &str, id: &str) -> Result<()> {
    if initial {
        return Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            format!("unknown {kind} wait target"),
        )
        .details(json!({"target_id":id})));
    }
    observation.unavailable = true;
    Ok(())
}
fn terminal(state: &str) -> bool {
    matches!(state, "completed" | "stopped" | "failed" | "exited")
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}
fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("wait database observation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DataOwner;

    async fn fixture() -> (tempfile::TempDir, DataOwner, Arc<Control>) {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(directory.path().join("data")),
            directory.path().to_path_buf(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, directory.path().to_path_buf());
        control.store.database.call(|db| {
            let tx=db.transaction()?;
            for (thread,run,turn,shell) in [("parent","rp","tp","sp"),("a","ra","ta","sa"),("b","rb","tb","sb")] {
                tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','/AGENTS.md','','','now')",[thread])?;
                tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?2,'live','now')",rusqlite::params![run,thread])?;
                tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES(?1,?2,?3,'running','now')",rusqlite::params![turn,thread,run])?;
                tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,?3,?1,'running','/','{}','UTF-8','UTF-8','open','now')",rusqlite::params![shell,run,turn])?;
            }
            tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('sa2','ra','ta','sa2','running','/','{}','UTF-8','UTF-8','open','now')",[])?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        (directory, owner, control)
    }
    fn origin(thread: &str, run: &str, turn: &str, shell: &str) -> Origin {
        Origin {
            instance_id: "fixture".into(),
            thread_id: thread.into(),
            run_id: run.into(),
            turn_id: turn.into(),
            execution_id: shell.into(),
            token: "fixture".into(),
        }
    }
    fn args(condition: WaitCondition, timeout_ms: u64) -> WaitArgs {
        WaitArgs {
            after: None,
            conditions: vec![condition],
            timeout_ms,
        }
    }
    async fn registered(control: &Control) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while control.waits.entries.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn pending_source_snapshot_is_exact_bounded_and_guard_owned() {
        let (_dir, owner, control) = fixture().await;
        assert_eq!(control.pending_wait_sources(), Some(vec![]));
        {
            let mut entries = control.waits.entries.lock().unwrap();
            entries.insert(
                "z:operation".into(),
                Registration {
                    source: "shell:execution:日本".into(),
                },
            );
            entries.insert(
                "a:operation".into(),
                Registration {
                    source: "shell:shell:nested".into(),
                },
            );
        }
        assert_eq!(
            control.pending_wait_sources(),
            Some(vec![
                ("a:operation".into(), "shell:nested".into()),
                ("z:operation".into(), "execution:日本".into())
            ])
        );
        let guard = Guard {
            entries: control.waits.entries.clone(),
            id: "a:operation".into(),
        };
        drop(guard);
        assert_eq!(control.pending_wait_sources().unwrap().len(), 1);
        {
            let mut entries = control.waits.entries.lock().unwrap();
            for index in 0..127 {
                entries.insert(
                    format!("operation-{index}"),
                    Registration {
                        source: format!("shell:execution-{index}"),
                    },
                );
            }
        }
        assert_eq!(control.pending_wait_sources().unwrap().len(), 128);
        control.waits.entries.lock().unwrap().insert(
            "overflow".into(),
            Registration {
                source: "shell:last".into(),
            },
        );
        assert!(
            control.pending_wait_sources().is_none(),
            "overflow is unavailable, never a partial/global-prefix snapshot"
        );
        control.waits.entries.lock().unwrap().remove("overflow");
        assert_eq!(control.pending_wait_sources().unwrap().len(), 128);
        control.waits.entries.lock().unwrap().clear();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn background_wait_creation_and_cleanup_notify_committed_facts_without_turn_changes() {
        let (_dir, owner, control) = fixture().await;
        let mut events = control.subscribe();
        let cancel = CancellationToken::new();
        let task = spawn_wait(
            &control,
            "background-visible",
            Some(origin("a", "ra", "ta", "sa")),
            cancel.clone(),
        );
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(60),events.recv()).await.unwrap().unwrap(),super::super::EngineEvent::Changed {thread_id,recovery:false} if thread_id=="a")
        );
        assert_eq!(
            control.pending_wait_sources(),
            Some(vec![("background-visible".into(), "sa".into())])
        );
        owner
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM app_meta WHERE key='wait.pending.background-visible'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    1
                );
                assert_eq!(
                    db.query_row("SELECT state FROM turn WHERE id='ta'", [], |row| row
                        .get::<_, String>(0))?,
                    "running"
                );
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        cancel.cancel();
        assert_eq!(task.await.unwrap().unwrap()["status"], "interrupted");
        assert!(
            matches!(events.try_recv(),Ok(super::super::EngineEvent::Changed {thread_id,recovery:false}) if thread_id=="a")
        );
        assert_eq!(control.pending_wait_sources(), Some(vec![]));
        owner
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM app_meta WHERE key='wait.pending.background-visible'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    db.query_row("SELECT state FROM turn WHERE id='ta'", [], |row| row
                        .get::<_, String>(0))?,
                    "running"
                );
                assert_eq!(
                    db.query_row(
                        "SELECT state FROM shell_execution WHERE id='sa2'",
                        [],
                        |row| row.get::<_, String>(0)
                    )?,
                    "running"
                );
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_pending_insert_or_delete_does_not_publish_a_rolled_back_fact() {
        for fail_insert in [true, false] {
            let (_dir, owner, control) = fixture().await;
            let mut events = control.subscribe();
            if fail_insert {
                owner.db.call(|db| {db.execute_batch("CREATE TRIGGER reject_pending BEFORE INSERT ON app_meta WHEN NEW.key LIKE 'wait.pending.%' BEGIN SELECT RAISE(ABORT,'synthetic pending insertion failure'); END;")?;Ok(())}).await.unwrap();
                let error = observe(
                    control.clone(),
                    "failed-pending".into(),
                    args(
                        WaitCondition::ShellEnded {
                            execution_id: "sa2".into(),
                        },
                        30_000,
                    ),
                    Some(origin("a", "ra", "ta", "sa")),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
                assert_eq!(error.code, ErrorCode::SaveFailed);
            } else {
                let cancel = CancellationToken::new();
                let task = spawn_wait(
                    &control,
                    "failed-pending",
                    Some(origin("a", "ra", "ta", "sa")),
                    cancel.clone(),
                );
                assert!(
                    matches!(tokio::time::timeout(Duration::from_secs(60),events.recv()).await.unwrap().unwrap(),super::super::EngineEvent::Changed {thread_id,recovery:false} if thread_id=="a")
                );
                owner.db.call(|db| {db.execute_batch("CREATE TRIGGER reject_pending BEFORE DELETE ON app_meta WHEN OLD.key LIKE 'wait.pending.%' BEGIN SELECT RAISE(ABORT,'synthetic pending cleanup failure'); END;")?;Ok(())}).await.unwrap();
                cancel.cancel();
                assert_eq!(task.await.unwrap().unwrap_err().code, ErrorCode::SaveFailed);
            }
            assert!(matches!(
                events.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
            assert_eq!(
                control.pending_wait_sources(),
                Some(vec![]),
                "Guard cleanup is independent of the failed DB mutation"
            );
            owner
                .db
                .call(move |db| {
                    assert_eq!(
                        db.query_row(
                            "SELECT count(*) FROM app_meta WHERE key='wait.pending.failed-pending'",
                            [],
                            |row| row.get::<_, i64>(0)
                        )?,
                        i64::from(!fail_insert)
                    );
                    assert_eq!(
                        db.query_row("SELECT state FROM turn WHERE id='ta'", [], |row| row
                            .get::<_, String>(0))?,
                        "running"
                    );
                    Ok(())
                })
                .await
                .unwrap();
            owner.shutdown().await.unwrap();
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn self_ancestor_and_composed_waits_are_observations_without_cycle_policy() {
        let (_dir, owner, control) = fixture().await;
        let a = origin("a", "ra", "ta", "sa");
        for condition in [
            WaitCondition::RunEnded {
                run_id: "rp".into(),
            },
            WaitCondition::TurnCompleted {
                turn_id: "tp".into(),
            },
            WaitCondition::ShellEnded {
                execution_id: "sa".into(),
            },
        ] {
            let result = observe(
                control.clone(),
                uuid::Uuid::new_v4().to_string(),
                args(condition, 0),
                Some(a.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(result["status"], "timeout");
        }
        let other = observe(
            control.clone(),
            "other-shell".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa2".into(),
                },
                0,
            ),
            Some(a.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(other["status"], "timeout");
        let cancel = CancellationToken::new();
        let first_control = control.clone();
        let first_cancel = cancel.clone();
        let first = tokio::spawn(async move {
            observe(
                first_control,
                "a-waits-b".into(),
                args(
                    WaitCondition::ShellEnded {
                        execution_id: "sb".into(),
                    },
                    5000,
                ),
                Some(a),
                first_cancel,
            )
            .await
        });
        registered(&control).await;
        let result = observe(
            control.clone(),
            "b-waits-a".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa".into(),
                },
                0,
            ),
            Some(origin("b", "rb", "tb", "sb")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "timeout");
        cancel.cancel();
        assert_eq!(first.await.unwrap().unwrap()["status"], "interrupted");
        assert!(control.waits.entries.lock().unwrap().is_empty());
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completion_registration_any_snapshot_recovery_deletion_and_disconnect_preserve_targets()
     {
        let (_dir, owner, control) = fixture().await;
        let cancel = CancellationToken::new();
        let waiting_control = control.clone();
        let observer_cancel = cancel.clone();
        let waiting = tokio::spawn(async move {
            observe(
                waiting_control,
                "completion".into(),
                args(
                    WaitCondition::TurnCompleted {
                        turn_id: "tb".into(),
                    },
                    5000,
                ),
                Some(origin("a", "ra", "ta", "sa")),
                observer_cancel,
            )
            .await
        });
        registered(&control).await;
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE turn SET state='completed' WHERE id='tb'", [])?;
                db.execute("UPDATE run SET state='completed' WHERE id='rb'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        control.changed("b");
        assert_eq!(waiting.await.unwrap().unwrap()["status"], "satisfied");
        let any = observe(
            control.clone(),
            "any".into(),
            WaitArgs {
                after: None,
                conditions: vec![
                    WaitCondition::TurnCompleted {
                        turn_id: "tb".into(),
                    },
                    WaitCondition::RunEnded {
                        run_id: "rb".into(),
                    },
                ],
                timeout_ms: 0,
            },
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(any["matched"].as_array().unwrap().len(), 2);
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE turn SET state='recovery_wait' WHERE id='ta'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let recovery = observe(
            control.clone(),
            "recovery".into(),
            args(
                WaitCondition::TurnCompleted {
                    turn_id: "ta".into(),
                },
                0,
            ),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(recovery["status"], "recovery_wait");
        control
            .store
            .database
            .call(|db| {
                db.execute("UPDATE thread SET deleted_at='now' WHERE id='b'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let deleted = observe(
            control.clone(),
            "deleted".into(),
            args(
                WaitCondition::RunEnded {
                    run_id: "rb".into(),
                },
                0,
            ),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(deleted["status"], "unavailable");
        let unknown = observe(
            control.clone(),
            "unknown".into(),
            args(
                WaitCondition::RunEnded {
                    run_id: "missing".into(),
                },
                0,
            ),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.code, ErrorCode::TargetUnavailable);
        let disconnected = CancellationToken::new();
        disconnected.cancel();
        let disconnected = observe(
            control.clone(),
            "disconnected".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa2".into(),
                },
                5000,
            ),
            None,
            disconnected,
        )
        .await
        .unwrap();
        assert_eq!(disconnected["status"], "interrupted");
        control
            .store
            .database
            .call(|db| {
                assert_eq!(
                    db.query_row(
                        "SELECT state FROM shell_execution WHERE id='sa2'",
                        [],
                        |row| row.get::<_, String>(0)
                    )?,
                    "running"
                );
                Ok(())
            })
            .await
            .unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_wait_is_durable_without_token_and_removed_on_disconnect_or_save_failure() {
        let (_dir, owner, control) = fixture().await;
        let disconnected = CancellationToken::new();
        let task = tokio::spawn(observe(
            control.clone(),
            "durable".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa2".into(),
                },
                30000,
            ),
            Some(origin("a", "ra", "ta", "sa")),
            disconnected.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let saved = control
                    .store
                    .database
                    .call(|db| {
                        Ok(db
                            .query_row(
                                "SELECT value FROM app_meta WHERE key='wait.pending.durable'",
                                [],
                                |r| r.get::<_, String>(0),
                            )
                            .optional()?)
                    })
                    .await
                    .unwrap();
                if let Some(body) = saved {
                    let body: Value = serde_json::from_str(&body).unwrap();
                    assert_eq!(body["caller"]["execution_id"], "sa");
                    assert!(body["caller"].get("token").is_none());
                    assert_eq!(body["conditions"][0]["execution_id"], "sa2");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        disconnected.cancel();
        assert_eq!(task.await.unwrap().unwrap()["status"], "interrupted");
        assert!(control.waits.entries.lock().unwrap().is_empty());
        control.store.database.call(|db|{
            assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",[],|r|r.get::<_,i64>(0))?,0);
            assert_eq!(db.query_row("SELECT state FROM shell_execution WHERE id='sa2'",[],|r|r.get::<_,String>(0))?,"running");
            db.execute_batch("CREATE TRIGGER reject_wait_snapshot BEFORE INSERT ON app_meta WHEN NEW.key LIKE 'wait.pending.%' BEGIN SELECT RAISE(ABORT,'injected wait snapshot failure'); END;")?;Ok(())
        }).await.unwrap();
        let error = observe(
            control.clone(),
            "failed-save".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa2".into(),
                },
                30000,
            ),
            Some(origin("a", "ra", "ta", "sa")),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SaveFailed);
        assert!(control.waits.entries.lock().unwrap().is_empty());
        owner.shutdown().await.unwrap();
    }
    async fn foreground_fixture() -> (
        tempfile::TempDir,
        DataOwner,
        Arc<Control>,
        Arc<ForegroundSlot>,
        super::super::foreground::ForegroundReceiver,
    ) {
        let (directory, owner, control) = fixture().await;
        control.store.database.call(|db| {
            db.execute_batch("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('attempt','ta',0,0,'','','0','completed','{}','now'); INSERT INTO tool_group VALUES('group','attempt','ta','running','now'); INSERT INTO tool_call VALUES('group','call',0,'shell','{}','running','sa');")?;
            Ok(())
        }).await.unwrap();
        let foreground = Arc::new(ForegroundSlot::default());
        let receive = foreground.register("sa");
        control.live.test_drivers().lock().await.insert(
            "a".into(),
            super::super::DriverSlot {
                id: "fixture".into(),
                run_id: "ra".into(),
                stop: CancellationToken::new(),
                nudge: Arc::new(tokio::sync::Notify::new()),
                foreground: foreground.clone(),
                turn: Arc::new(tokio::sync::Mutex::new(None)),
            },
        );
        (directory, owner, control, foreground, receive)
    }
    async fn pending_count(control: &Control, expected: i64) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let count = control
                    .store
                    .database
                    .call(|db| {
                        Ok(db.query_row(
                            "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                            [],
                            |r| r.get::<_, i64>(0),
                        )?)
                    })
                    .await
                    .unwrap();
                if count == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn waiting_state(control: &Control, expected: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = control
                    .store
                    .database
                    .call(|db| {
                        Ok(
                            db.query_row("SELECT state FROM turn WHERE id='ta'", [], |r| {
                                r.get::<_, String>(0)
                            })?,
                        )
                    })
                    .await
                    .unwrap();
                if state == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    fn spawn_wait(
        control: &Arc<Control>,
        id: &str,
        caller: Option<Origin>,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<Result<Value>> {
        let control = control.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            observe(
                control,
                id,
                args(
                    WaitCondition::ShellEnded {
                        execution_id: "sa2".into(),
                    },
                    30000,
                ),
                caller,
                cancel,
            )
            .await
        })
    }
    #[tokio::test]
    async fn last_foreground_wait_restores_running_but_human_and_background_observers_do_not_change_it()
     {
        let (_directory, owner, control, foreground, _receive) = foreground_fixture().await;
        let caller = origin("a", "ra", "ta", "sa");
        control.store.database.call(|db| {db.execute_batch("CREATE TRIGGER forbid_observer_wait BEFORE UPDATE OF state ON turn WHEN NEW.state='waiting' BEGIN SELECT RAISE(ABORT,'observer cannot pause the model'); END;")?;Ok(())}).await.unwrap();
        for source in [None, Some(origin("a", "ra", "ta", "sa2"))] {
            let cancel = CancellationToken::new();
            // A background shell can observe another live shell without being
            // the execution that blocks the model. Avoid the own-shell cycle.
            let task = if source.is_some() {
                let control = control.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    observe(
                        control,
                        "background".into(),
                        args(
                            WaitCondition::ShellEnded {
                                execution_id: "sa".into(),
                            },
                            30000,
                        ),
                        source,
                        cancel,
                    )
                    .await
                })
            } else {
                spawn_wait(&control, "human", source, cancel.clone())
            };
            pending_count(&control, 1).await;
            waiting_state(&control, "running").await;
            cancel.cancel();
            assert_eq!(task.await.unwrap().unwrap()["status"], "interrupted");
        }
        control
            .store
            .database
            .call(|db| {
                db.execute_batch("DROP TRIGGER forbid_observer_wait;")?;
                Ok(())
            })
            .await
            .unwrap();
        let first_cancel = CancellationToken::new();
        let second_cancel = CancellationToken::new();
        let first = spawn_wait(
            &control,
            "first",
            Some(caller.clone()),
            first_cancel.clone(),
        );
        let second = spawn_wait(&control, "second", Some(caller), second_cancel.clone());
        waiting_state(&control, "waiting").await;
        pending_count(&control, 2).await;
        // Barrier both state bindings, not just the earlier metadata INSERT.
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let count=control.store.database.call(|db| Ok(db.query_row("SELECT COUNT(*) FROM app_meta WHERE key LIKE 'wait.pending.%' AND json_type(value,'$.turn_wait_generation')='integer'",[],|r|r.get::<_,i64>(0))?)).await.unwrap();
                if count==2 {break;} tokio::task::yield_now().await;
            }
        }).await.unwrap();
        first_cancel.cancel();
        first.await.unwrap().unwrap();
        waiting_state(&control, "waiting").await;
        second_cancel.cancel();
        second.await.unwrap().unwrap();
        waiting_state(&control, "running").await;
        pending_count(&control, 0).await;
        foreground.clear();
        control.live.test_drivers().lock().await.clear();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn old_wait_cleanup_does_not_resume_a_new_generation_or_terminal_turn() {
        for next in ["generation", "recovery_wait", "stopped"] {
            let (_directory, owner, control, foreground, _receive) = foreground_fixture().await;
            let cancel = CancellationToken::new();
            let task = spawn_wait(
                &control,
                "old",
                Some(origin("a", "ra", "ta", "sa")),
                cancel.clone(),
            );
            waiting_state(&control, "waiting").await;
            let updated = next.to_owned();
            control
                .store
                .database
                .call(move |db| {
                    if updated == "generation" {
                        db.execute_batch("UPDATE turn SET generation=1 WHERE id='ta';")?;
                    } else {
                        db.execute("UPDATE turn SET state=?1 WHERE id='ta'", [updated])?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
            cancel.cancel();
            task.await.unwrap().unwrap();
            let expected = if next == "generation" {
                "waiting"
            } else {
                next
            };
            waiting_state(&control, expected).await;
            pending_count(&control, 0).await;
            foreground.clear();
            control.live.test_drivers().lock().await.clear();
            owner.shutdown().await.unwrap();
        }
    }
    #[tokio::test]
    async fn wait_registered_before_foreground_slot_is_reobserved_and_immediate_results_never_pause()
     {
        let (_directory, owner, control, foreground, _receive) = foreground_fixture().await;
        control.store.database.call(|db| {db.execute_batch("CREATE TRIGGER forbid_immediate_wait BEFORE UPDATE OF state ON turn WHEN NEW.state='waiting' BEGIN SELECT RAISE(ABORT,'immediate result cannot pause'); END;")?;Ok(())}).await.unwrap();
        let caller = origin("a", "ra", "ta", "sa");
        let result = observe(
            control.clone(),
            "zero".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa2".into(),
                },
                0,
            ),
            Some(caller.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "timeout");
        control.store.database.call(|db| {db.execute_batch("UPDATE shell_execution SET state='exited',exit_code=0,ended_at='now',output_complete=1 WHERE id='sa2';")?;Ok(())}).await.unwrap();
        let result = observe(
            control.clone(),
            "ready".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa2".into(),
                },
                30000,
            ),
            Some(caller.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "satisfied");
        control.store.database.call(|db| {db.execute_batch("DROP TRIGGER forbid_immediate_wait; UPDATE shell_execution SET state='running',exit_code=NULL,ended_at=NULL,output_complete=NULL WHERE id='sa2';")?;Ok(())}).await.unwrap();
        foreground.clear();
        let cancel = CancellationToken::new();
        let task = spawn_wait(&control, "early", Some(caller), cancel.clone());
        pending_count(&control, 1).await;
        waiting_state(&control, "running").await;
        let _current = foreground.register("sa");
        control.changed.notify_waiters();
        waiting_state(&control, "waiting").await;
        cancel.cancel();
        task.await.unwrap().unwrap();
        waiting_state(&control, "running").await;
        foreground.clear();
        control.live.test_drivers().lock().await.clear();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn wait_state_save_failure_rolls_back_binding_and_releases_registration() {
        let (_directory, owner, control, foreground, _receive) = foreground_fixture().await;
        control.store.database.call(|db| {
            db.execute_batch("CREATE TRIGGER fail_wait_state BEFORE UPDATE OF state ON turn WHEN NEW.state='waiting' BEGIN SELECT RAISE(ABORT,'fixture waiting save failure'); END;")?;
            Ok(())
        }).await.unwrap();
        let result = observe(
            control.clone(),
            "failed-state".into(),
            args(
                WaitCondition::ShellEnded {
                    execution_id: "sa2".into(),
                },
                30000,
            ),
            Some(origin("a", "ra", "ta", "sa")),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(result.code, ErrorCode::SaveFailed);
        waiting_state(&control, "running").await;
        pending_count(&control, 0).await;
        assert!(control.waits.entries.lock().unwrap().is_empty());
        foreground.clear();
        control.live.test_drivers().lock().await.clear();
        owner.shutdown().await.unwrap();
    }
    /// One parked `wait` stays parked through a client looping `events.wait` with text.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_looping_text_client_does_not_reobserve_parked_waits() {
        let (_directory, owner, control) = fixture().await;
        let watching = control.clone();
        let observer = tokio::spawn(async move {
            observe(
                watching,
                "parked-through-text".into(),
                args(
                    WaitCondition::TurnCompleted {
                        turn_id: "ta".into(),
                    },
                    600_000,
                ),
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap()
        });
        pending_count(&control, 1).await;
        let poll = |after: Option<String>| {
            Request::new(
                uuid::Uuid::new_v4().to_string(),
                Command::EventsWait(EventsWaitArgs {
                    after,
                    thread_ids: vec!["streaming".into()],
                    timeout_ms: 600_000,
                    include_text: true,
                    include_reasoning_activity: false,
                }),
            )
        };
        let first = control.request(poll(None)).await.result.unwrap();
        let mut at = first["cursor"].as_str().unwrap().to_owned();
        let before = control
            .waits
            .observations
            .load(std::sync::atomic::Ordering::Relaxed);
        for offset in 0..200 {
            control.stream_delta("streaming", "attempt", "x", false, offset);
            let value = control
                .request(poll(Some(at.clone())))
                .await
                .result
                .unwrap();
            assert_eq!(value["status"], "text");
            at = value["cursor"].as_str().unwrap().to_owned();
        }
        assert_eq!(
            control
                .waits
                .observations
                .load(std::sync::atomic::Ordering::Relaxed),
            before,
            "text deltas and events.wait completions must not re-observe a parked wait"
        );
        // A saved change still reaches it.
        owner
            .db
            .call(|db| {
                db.execute_batch("UPDATE turn SET state='completed' WHERE id='ta'")?;
                Ok(())
            })
            .await
            .unwrap();
        control.changed("a");
        let value = crate::test_watchdog::watchdog("parked wait", observer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["status"], "satisfied");
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_notifications_do_not_reobserve_lifecycle_waits_but_terminal_changes_do() {
        for case in 0..4 {
            let (_directory, owner, control) = fixture().await;
            let condition = match case {
                0 => WaitCondition::ShellEnded {
                    execution_id: "sa".into(),
                },
                1 | 3 => WaitCondition::TurnCompleted {
                    turn_id: "ta".into(),
                },
                _ => WaitCondition::RunEnded {
                    run_id: "ra".into(),
                },
            };
            let watching = control.clone();
            let mut observer = tokio::spawn(async move {
                observe(
                    watching,
                    format!("lifecycle-{case}"),
                    args(condition, 30_000),
                    None,
                    CancellationToken::new(),
                )
                .await
                .unwrap()
            });
            pending_count(&control, 1).await;
            let before = control
                .waits
                .observations
                .load(std::sync::atomic::Ordering::Relaxed);
            for _ in 0..100 {
                control.shell_output_changed.notify_waiters();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut observer)
                    .await
                    .is_err()
            );
            assert_eq!(
                control
                    .waits
                    .observations
                    .load(std::sync::atomic::Ordering::Relaxed),
                before,
                "output-only notifications must not enqueue a lifecycle observation"
            );
            owner.db.call(move |db| {
                db.execute_batch(match case {
                    0 => "UPDATE shell_execution SET state='exited',output_complete=1,exit_code=0 WHERE id='sa'",
                    1 => "UPDATE turn SET state='completed' WHERE id='ta'",
                    2 => "UPDATE run SET state='completed' WHERE id='ra'",
                    _ => "UPDATE turn SET state='recovery_wait' WHERE id='ta'",
                })?; Ok(())
            }).await.unwrap();
            control.changed("a");
            let value = tokio::time::timeout(Duration::from_secs(1), observer)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                value["status"],
                if case == 3 {
                    "recovery_wait"
                } else {
                    "satisfied"
                }
            );
            owner.shutdown().await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_condition_in_any_wait_observes_committed_bytes_without_lifecycle_change() {
        let (_directory, owner, control) = fixture().await;
        let reader = super::super::output::ShellOutputReader::new(
            owner.db.clone(),
            control.store.instance_id.clone(),
        );
        let empty = reader
            .read(ShellReadArgs {
                execution_id: "sa".into(),
                cursor: None,
                encoding: Some("UTF-8".into()),
                limit_bytes: 100,
            })
            .await
            .unwrap();
        let query = WaitArgs {
            after: None,
            conditions: vec![
                WaitCondition::ShellEnded {
                    execution_id: "sa".into(),
                },
                WaitCondition::ShellOutput {
                    execution_id: "sa".into(),
                    cursor: empty.cursor,
                },
            ],
            timeout_ms: 30_000,
        };
        let watching = control.clone();
        let observer = tokio::spawn(async move {
            observe(
                watching,
                "output-any".into(),
                query,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap()
        });
        pending_count(&control, 1).await;
        owner.db.call(|db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('sa',0,'stdout',0,x'5245414459','now')",[])?;
            tx.execute("UPDATE shell_execution SET saved_sequence=1 WHERE id='sa'",[])?;
            tx.commit()?; Ok(())
        }).await.unwrap();
        control.shell_output_changed.notify_waiters();
        let value = tokio::time::timeout(Duration::from_secs(1), observer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value["status"], "satisfied");
        assert_eq!(value["matched"].as_array().unwrap().len(), 1);
        assert_eq!(value["matched"][0]["type"], "shell_output");
        // The same condition already satisfied before subscription must return
        // from its initial inspection without needing a saved Notify permit.
        let read = reader
            .read(ShellReadArgs {
                execution_id: "sa".into(),
                cursor: None,
                encoding: Some("UTF-8".into()),
                limit_bytes: 1,
            })
            .await
            .unwrap();
        let immediate = observe(
            control,
            "already-output".into(),
            args(
                WaitCondition::ShellOutput {
                    execution_id: "sa".into(),
                    cursor: read.cursor,
                },
                0,
            ),
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(immediate["status"], "satisfied");
        owner.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod thread_tests;

#[cfg(test)]
mod reply_received_tests {
    use super::*;
    #[test]
    fn pending_reply_has_no_deadline_and_all_terminal_reasons_satisfy() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','0'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('m','user','0','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,result_json)VALUES('m','op','{}','t','m','held','0','{\"expect_reply\":true,\"reply\":{\"state\":\"pending\"}}');").unwrap();
        let args = WaitArgs {
            conditions: vec![WaitCondition::ReplyReceived {
                message_id: "m".into(),
            }],
            timeout_ms: 0,
            after: None,
        };
        let pending = inspect(&db, &args, None, true).unwrap();
        assert!(pending.matched.is_empty());
        assert!(!pending.recovery);
        assert_eq!(
            outcome(&args, &pending, Some(WaitStatus::Timeout))
                .unwrap()
                .unwrap()["status"],
            "timeout"
        );
        for state in ["replied", "ignored", "cancelled", "recipient_deleted"] {
            db.execute(
                "UPDATE receipt SET result_json=json_set(result_json,'$.reply.state',?1,'$.reply.by',json('{\"kind\":\"model\",\"thread_id\":\"responder\"}'))",
                [state],
            )
            .unwrap();
            let terminal = inspect(&db, &args, None, true).unwrap();
            assert_eq!(terminal.matched, args.conditions);
            assert_eq!(terminal.snapshot["conditions"][0]["state"], state);
            assert_eq!(
                terminal.snapshot["conditions"][0]["by"]["thread_id"],
                "responder"
            );
            assert_eq!(
                outcome(&args, &terminal, Some(WaitStatus::Interrupted))
                    .unwrap()
                    .unwrap()["status"],
                "satisfied"
            );
        }
        db.execute("UPDATE receipt SET result_json='{}'", [])
            .unwrap();
        assert_eq!(
            inspect(&db, &args, None, true).err().unwrap().code,
            ErrorCode::InvalidArgument
        );
    }
}
