use super::Control;
use crate::{
    protocol::ShellToolArgs,
    shell::{ShellEvent, ShellSupervisor},
    storage::schedule::{self, DueSchedule},
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

const MAX_ACTIVE_SCHEDULES: usize = 4;
const OUTPUT_TAIL_BYTES: usize = 4096;

pub(super) async fn run(control: Weak<Control>, stop: CancellationToken) {
    let mut active = HashSet::new();
    let mut children = JoinSet::new();
    loop {
        while let Some(result) = children.try_join_next() {
            if let Ok(Ok(id)) = result {
                active.remove(&id);
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
            Err(_) => {
                tokio::select! { _=stop.cancelled()=>break, _=&mut changed=>{} };
                continue;
            }
        };
        match deadline {
            None => {
                tokio::select! { _=stop.cancelled()=>break, _=&mut changed=>{}, child=children.join_next(), if !children.is_empty()=>if let Some(Ok(Ok(id)))=child { active.remove(&id); } }
            }
            Some(at) => {
                let now = chrono::Utc::now().timestamp();
                if at > now {
                    tokio::select! { _=stop.cancelled()=>break, _=&mut changed=>{}, _=sleep(Duration::from_secs((at-now) as u64))=>{} }
                }
                loop {
                    if stop.is_cancelled() {
                        break;
                    }
                    let Some(owner) = control.upgrade() else {
                        break;
                    };
                    let now = chrono::Utc::now().timestamp();
                    let due = match schedule::advance_due(&owner.store.database, now).await {
                        Ok(v) => v,
                        Err(_) => None,
                    };
                    let Some(due) = due else { break };
                    if active.contains(&due.id) {
                        let _ = schedule::finish(
                            &owner.store.database,
                            due.id,
                            due.scheduled_at,
                            now,
                            "overlap_skipped",
                            None,
                            None,
                            false,
                        )
                        .await;
                    } else if active.len() >= MAX_ACTIVE_SCHEDULES {
                        let _ = schedule::finish(
                            &owner.store.database,
                            due.id,
                            due.scheduled_at,
                            now,
                            "capacity_skipped",
                            None,
                            None,
                            false,
                        )
                        .await;
                    } else {
                        active.insert(due.id);
                        children.spawn(execute(owner.clone(), due, stop.clone()));
                    }
                }
            }
        }
    }
    for child in children.join_all().await {
        if let Ok(id) = child {
            active.remove(&id);
        }
    }
}

async fn execute(
    owner: Arc<Control>,
    due: DueSchedule,
    stop: CancellationToken,
) -> Result<uuid::Uuid, ()> {
    let id = due.id;
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
            std::collections::BTreeMap::new(),
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
            let mut stop_sent = false;
            loop {
                tokio::select! { biased;
                    _=stop.cancelled(), if !stop_sent=>{execution.handle.stop();stop_sent=true;},
                    event=execution.events.recv(), if events_open=>match event {Some(ShellEvent::Output(chunk))=>for byte in chunk.bytes.iter().copied() { if tail.len()==OUTPUT_TAIL_BYTES {tail.pop_front();truncated=true;} tail.push_back(byte); },Some(ShellEvent::Stdin{..})=>{},None=>events_open=false},
                    changed=execution.completion.changed()=>{ if changed.is_err() { break; } },
                }
                if execution.completion.borrow().is_some() {
                    break;
                }
            }
            let Some(completion) = execution.completion.borrow().clone() else {
                return Err(());
            };
            let completion = completion;
            let mut output =
                String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned();
            while output.len() > OUTPUT_TAIL_BYTES {
                output.remove(0);
                truncated = true;
            }
            (
                if completion.state == crate::protocol::ShellState::Exited {
                    "finished"
                } else {
                    "launch_failed"
                },
                completion.exit_code,
                (!output.is_empty()).then_some(output),
                truncated,
            )
        }
    };
    let _ = schedule::finish(
        &owner.store.database,
        id,
        due.scheduled_at,
        chrono::Utc::now().timestamp().max(started),
        status,
        code,
        tail,
        truncated,
    )
    .await;
    owner.changed.notify_waiters();
    Ok(id)
}
