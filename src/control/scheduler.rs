use super::Control;
use crate::{
    protocol::ShellToolArgs,
    shell::{ShellEvent, ShellSupervisor},
    storage::schedule::{self, DueSchedule, ScheduledCommandResult},
};
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Weak},
};
use tokio::{
    task::JoinSet,
    time::{Duration, sleep},
};
use tokio_util::sync::CancellationToken;

const OUTPUT_TAIL_BYTES: usize = 4096;

fn next_sleep_seconds(next_at: i64, now: i64, max_sleep_seconds: u64) -> Option<u64> {
    let remaining = next_at.saturating_sub(now);
    (remaining > 0 && max_sleep_seconds > 0)
        .then(|| remaining.min(max_sleep_seconds.min(i64::MAX as u64) as i64) as u64)
}

#[cfg(test)]
#[test]
fn synthetic_wall_clock_recheck_recomputes_due_state_after_sleep_bound() {
    assert_eq!(next_sleep_seconds(600, 0, 60), Some(60));
    assert_eq!(next_sleep_seconds(600, 500, 60), Some(60));
    assert_eq!(next_sleep_seconds(600, 700, 60), None);
    assert_eq!(next_sleep_seconds(600, 500, 300), Some(100));
}

pub(super) async fn run(control: Weak<Control>, stop: CancellationToken) {
    let mut active = HashSet::new();
    let mut children = JoinSet::new();
    'dispatcher: loop {
        while let Some(result) = children.try_join_next() {
            if let Ok(id) = result {
                active.remove(&id);
                if let Some(owner) = control.upgrade() {
                    owner
                        .scheduled_active
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .remove(&id);
                }
            }
        }
        let Some(owner) = control.upgrade() else {
            break;
        };
        if stop.is_cancelled() {
            break;
        }
        let changed = owner.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let deadline = match schedule::next_deadline(&owner.store.database).await {
            Ok(deadline) => deadline,
            Err(storage) => {
                let error = storage.into_api_error();
                if error.is_storage_failure() {
                    owner.fail_storage().await;
                    break 'dispatcher;
                }
                tokio::select! { _=stop.cancelled()=>break 'dispatcher, _=&mut changed=>{} };
                continue;
            }
        };
        match deadline {
            None => {
                tokio::select! { _=stop.cancelled()=>break 'dispatcher, _=&mut changed=>{}, child=children.join_next(), if !children.is_empty()=>if let Some(Ok(id))=child { active.remove(&id); if let Some(owner)=control.upgrade(){owner.scheduled_active.lock().unwrap_or_else(|poison|poison.into_inner()).remove(&id);} } }
            }
            Some(at) => {
                let now = chrono::Utc::now().timestamp();
                let sleep_seconds = next_sleep_seconds(
                    at,
                    now,
                    owner
                        .schedule_max_sleep_seconds
                        .load(std::sync::atomic::Ordering::SeqCst),
                );
                if let Some(seconds) = sleep_seconds {
                    tokio::select! { _=stop.cancelled()=>break 'dispatcher, _=&mut changed=>{}, _=sleep(Duration::from_secs(seconds))=>{}, child=children.join_next(), if !children.is_empty()=>if let Some(Ok(id))=child { active.remove(&id); if let Some(owner)=control.upgrade(){owner.scheduled_active.lock().unwrap_or_else(|poison|poison.into_inner()).remove(&id);} } }
                } else if at > now {
                    tokio::select! { _=stop.cancelled()=>break 'dispatcher, _=&mut changed=>{}, child=children.join_next(), if !children.is_empty()=>if let Some(Ok(id))=child { active.remove(&id); if let Some(owner)=control.upgrade(){owner.scheduled_active.lock().unwrap_or_else(|poison|poison.into_inner()).remove(&id);} } }
                }
                loop {
                    while let Some(result) = children.try_join_next() {
                        if let Ok(id) = result {
                            active.remove(&id);
                            owner
                                .scheduled_active
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .remove(&id);
                        }
                    }
                    if stop.is_cancelled() {
                        break;
                    }
                    let Some(owner) = control.upgrade() else {
                        break;
                    };
                    let now = chrono::Utc::now().timestamp();
                    let due = match schedule::advance_due(&owner.store.database, now).await {
                        Ok(v) => v,
                        Err(storage) => {
                            let error = storage.into_api_error();
                            if error.is_storage_failure() {
                                owner.fail_storage().await;
                                break 'dispatcher;
                            }
                            tokio::select! { _=stop.cancelled()=>break 'dispatcher, _=&mut changed=>{} }
                            break;
                        }
                    };
                    let Some(due) = due else { break };
                    while let Some(result) = children.try_join_next() {
                        if let Ok(id) = result {
                            active.remove(&id);
                            owner
                                .scheduled_active
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .remove(&id);
                        }
                    }
                    if active.contains(&due.id) {
                        let _ = schedule::finish(
                            &owner.store.database,
                            due.id,
                            ScheduledCommandResult {
                                scheduled_at: due.scheduled_at,
                                finished_at: now,
                                status: "overlap_skipped".into(),
                                exit_code: None,
                                output_tail: None,
                                output_truncated: false,
                            },
                        )
                        .await;
                    } else {
                        active.insert(due.id);
                        let schedule_id = due.id;
                        let child_stop = stop.clone();
                        children.spawn(async move {
                            execute(owner, due, child_stop).await.unwrap_or(schedule_id)
                        });
                    }
                }
            }
        }
    }
    for id in children.join_all().await {
        active.remove(&id);
        if let Some(owner) = control.upgrade() {
            owner
                .scheduled_active
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
        }
    }
}

async fn execute(
    owner: Arc<Control>,
    due: DueSchedule,
    stop: CancellationToken,
) -> Result<uuid::Uuid, ()> {
    let id = due.id;
    let thread_id = due.thread_id.clone();
    if stop.is_cancelled() {
        return Ok(id);
    }
    let boundary = owner.boundary(&thread_id).await;
    if stop.is_cancelled() {
        return Ok(id);
    }
    let _admission = match boundary.try_lock_owned() {
        Ok(guard) => guard,
        Err(_) => {
            if stop.is_cancelled() {
                return Ok(id);
            }
            let now = chrono::Utc::now().timestamp();
            let _ = schedule::finish(
                &owner.store.database,
                id,
                ScheduledCommandResult {
                    scheduled_at: due.scheduled_at,
                    finished_at: now,
                    status: "launch_failed".into(),
                    exit_code: None,
                    output_tail: Some(
                        "Scheduler: Thread admission busy; command not started".into(),
                    ),
                    output_truncated: false,
                },
            )
            .await;
            return Ok(id);
        }
    };
    if stop.is_cancelled() {
        return Ok(id);
    }
    let target = thread_id.clone();
    let thread_exists = owner
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                [target],
                |row| row.get::<_, bool>(0),
            )?)
        })
        .await
        .map_err(|_| ())?;
    if !thread_exists {
        return Ok(id);
    }
    owner
        .scheduled_active
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(id, thread_id.clone());
    drop(_admission);
    let started = chrono::Utc::now().timestamp();
    let args = ShellToolArgs {
        command: due.command.clone(),
        description: None,
        background: false,
        stdin_open: false,
        initial_stdin: None,
        input_encoding: "utf-8".into(),
        output_encoding: "utf-8".into(),
    };
    let supervisor: ShellSupervisor = owner.shell.clone();
    let execution = supervisor
        .spawn(
            id.to_string(),
            PathBuf::from(due.cwd),
            args,
            [("SHELLBOY_THREAD_ID".to_string(), thread_id)]
                .into_iter()
                .collect(),
        )
        .await;
    let (status, code, tail, truncated) = match execution {
        Err(error) => (
            "launch_failed",
            None,
            Some(format!("Scheduler: launch failed: {error}")),
            false,
        ),
        Ok(mut execution) => {
            let mut tail = VecDeque::with_capacity(OUTPUT_TAIL_BYTES);
            let mut truncated = false;
            let mut events_open = true;
            let mut completion_open = true;
            let mut stop_sent = false;
            loop {
                tokio::select! { biased;
                    _=stop.cancelled(), if !stop_sent=>{execution.handle.stop();stop_sent=true;},
                    event=execution.events.recv(), if events_open=>match event {Some(ShellEvent::Output(chunk))=>for byte in chunk.bytes.iter().copied() { if tail.len()==OUTPUT_TAIL_BYTES {tail.pop_front();truncated=true;} tail.push_back(byte); },Some(ShellEvent::Stdin{..})=>{},None=>events_open=false},
                    changed=execution.completion.changed(), if completion_open=>{ if changed.is_err() { completion_open=false; } },
                }
                let completed = execution.completion.borrow().is_some();
                if completed && !events_open {
                    break;
                }
                if !completion_open && !completed {
                    break;
                }
            }
            let Some(completion) = execution.completion.borrow().clone() else {
                return Err(());
            };
            truncated |= !completion.output_complete;
            let mut output =
                String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned();
            while output.len() > OUTPUT_TAIL_BYTES {
                output.remove(0);
                truncated = true;
            }
            (
                "finished",
                completion.exit_code,
                (!output.is_empty()).then_some(output),
                truncated,
            )
        }
    };
    let _ = schedule::finish(
        &owner.store.database,
        id,
        ScheduledCommandResult {
            scheduled_at: due.scheduled_at,
            finished_at: chrono::Utc::now().timestamp().max(started),
            status: status.into(),
            exit_code: code,
            output_tail: tail,
            output_truncated: truncated,
        },
    )
    .await;
    Ok(id)
}
