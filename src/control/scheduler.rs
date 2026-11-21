use super::{ActiveScheduledCommand, Control};
use crate::{
    protocol::ShellToolArgs,
    shell::{ShellCompletion, ShellEvent, ShellExecution, ShellSupervisor},
    storage::schedule::{self, DueSchedule, ScheduledCommandResult},
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Weak},
};
use tokio::{
    task::JoinSet,
    time::{Duration, Instant, sleep, sleep_until},
};
use tokio_util::sync::CancellationToken;

const OUTPUT_TAIL_BYTES: usize = 4096;
const MAX_SLEEP_SECONDS: u64 = 60;
const MAX_RUN_DURATION: Duration = Duration::from_secs(60 * 60);

fn next_sleep_seconds(next_at: i64, now: i64) -> Option<u64> {
    let remaining = next_at.saturating_sub(now);
    (remaining > 0).then(|| remaining.min(MAX_SLEEP_SECONDS as i64) as u64)
}

struct ActiveScheduleGuard {
    owner: Arc<Control>,
    schedule_id: uuid::Uuid,
}
impl Drop for ActiveScheduleGuard {
    fn drop(&mut self) {
        self.owner
            .scheduled_active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&self.schedule_id);
    }
}

#[cfg(test)]
#[test]
fn fixed_wall_clock_recheck_is_sixty_seconds_and_stops_when_empty_or_due() {
    assert_eq!(MAX_SLEEP_SECONDS, 60);
    assert_eq!(next_sleep_seconds(600, 0), Some(60));
    assert_eq!(next_sleep_seconds(600, 500), Some(60));
    assert_eq!(next_sleep_seconds(600, 599), Some(1));
    assert_eq!(next_sleep_seconds(600, 700), None);
}

pub(super) async fn run(control: Weak<Control>, stop: CancellationToken) {
    let mut children = JoinSet::new();
    'dispatcher: loop {
        while children.try_join_next().is_some() {}
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
                tokio::select! {
                    _=stop.cancelled()=>break 'dispatcher,
                    _=&mut changed=>{},
                    _=children.join_next(), if !children.is_empty()=>{},
                }
            }
            Some(at) => {
                loop {
                    let now = chrono::Utc::now().timestamp();
                    let Some(seconds) = next_sleep_seconds(at, now) else {
                        break;
                    };
                    tokio::select! {
                        _=stop.cancelled()=>break 'dispatcher,
                        _=&mut changed=>continue 'dispatcher,
                        _=sleep(Duration::from_secs(seconds))=>{},
                        _=children.join_next(), if !children.is_empty()=>{},
                    }
                }
                loop {
                    while children.try_join_next().is_some() {}
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
                    let schedule_id = due.id;
                    let thread_id = due.thread_id.clone();
                    let overlap = {
                        let mut active = owner
                            .scheduled_active
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        if active.contains_key(&schedule_id) {
                            true
                        } else {
                            active.insert(
                                schedule_id,
                                ActiveScheduledCommand {
                                    thread_id,
                                    launched: false,
                                },
                            );
                            false
                        }
                    };
                    if overlap {
                        let _ = schedule::finish(
                            &owner.store.database,
                            schedule_id,
                            ScheduledCommandResult {
                                scheduled_at: due.scheduled_at,
                                finished_at: chrono::Utc::now().timestamp(),
                                status: "overlap_skipped".into(),
                                exit_code: None,
                                output_tail: None,
                                output_truncated: false,
                            },
                        )
                        .await;
                    } else {
                        let child_stop = stop.clone();
                        let active_guard = ActiveScheduleGuard {
                            owner: owner.clone(),
                            schedule_id,
                        };
                        children.spawn(async move {
                            let _active_guard = active_guard;
                            execute(owner, due, child_stop).await.unwrap_or(schedule_id)
                        });
                    }
                }
            }
        }
    }
    let _ = children.join_all().await;
}

struct CollectedShell {
    completion: ShellCompletion,
    output_tail: Option<String>,
    output_truncated: bool,
    timed_out: bool,
    finished_at: i64,
}

async fn collect_shell_output(
    mut execution: ShellExecution,
    stop: CancellationToken,
    run_limit: Duration,
) -> Result<CollectedShell, ()> {
    let deadline = Instant::now() + run_limit;
    let mut tail = VecDeque::with_capacity(OUTPUT_TAIL_BYTES);
    let mut truncated = false;
    let mut events_open = true;
    let mut completion_open = true;
    let mut stop_sent = false;
    let mut timed_out = false;
    let mut finished_at = None;
    loop {
        tokio::select! { biased;
            _=stop.cancelled(), if !stop_sent=>{execution.handle.stop();stop_sent=true;},
            event=execution.events.recv(), if events_open=>match event {Some(ShellEvent::Output(chunk))=>for byte in chunk.bytes.iter().copied() { if tail.len()==OUTPUT_TAIL_BYTES {tail.pop_front();truncated=true;} tail.push_back(byte); },Some(ShellEvent::Stdin{..})=>{},None=>events_open=false},
            changed=execution.completion.changed(), if completion_open=>{ if changed.is_err() { completion_open=false; } },
            _=sleep_until(deadline), if !stop_sent && execution.completion.borrow().is_none()=>{
                timed_out=true;
                execution.handle.stop();
                stop_sent=true;
            },
        }
        let completed = execution.completion.borrow().is_some();
        if completed && finished_at.is_none() {
            finished_at = Some(chrono::Utc::now().timestamp());
        }
        if completed && !events_open {
            break;
        }
        if !completion_open && !completed {
            return Err(());
        }
    }
    let Some(completion) = execution.completion.borrow().clone() else {
        return Err(());
    };
    truncated |= !completion.output_complete;
    let mut output = String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned();
    while output.len() > OUTPUT_TAIL_BYTES {
        output.remove(0);
        truncated = true;
    }
    Ok(CollectedShell {
        completion,
        output_tail: (!output.is_empty()).then_some(output),
        output_truncated: truncated,
        timed_out,
        finished_at: finished_at.unwrap_or_else(|| chrono::Utc::now().timestamp()),
    })
}

fn scheduled_shell_result(scheduled_at: i64, collected: CollectedShell) -> ScheduledCommandResult {
    ScheduledCommandResult {
        scheduled_at,
        finished_at: collected.finished_at,
        status: if collected.timed_out {
            "timed_out"
        } else {
            "finished"
        }
        .into(),
        exit_code: if collected.timed_out {
            None
        } else {
            collected.completion.exit_code
        },
        output_tail: collected.output_tail,
        output_truncated: collected.output_truncated,
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
    let admission = tokio::select! {
        biased;
        _=stop.cancelled()=>return Ok(id),
        guard=boundary.lock_owned()=>guard,
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
    let launched = execution.is_ok();
    if let Some(active) = owner
        .scheduled_active
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get_mut(&id)
    {
        active.launched = launched;
    }
    drop(admission);
    let result = match execution {
        Err(error) => ScheduledCommandResult {
            scheduled_at: due.scheduled_at,
            finished_at: chrono::Utc::now().timestamp(),
            status: "launch_failed".into(),
            exit_code: None,
            output_tail: Some(format!("Scheduler: launch failed: {error}")),
            output_truncated: false,
        },
        Ok(execution) => {
            let collected = collect_shell_output(execution, stop.clone(), MAX_RUN_DURATION).await?;
            scheduled_shell_result(due.scheduled_at, collected)
        }
    };
    let _ = schedule::finish(&owner.store.database, id, result).await;
    Ok(id)
}

#[cfg(all(test, unix))]
mod dst_behavior_tests {
    use chrono::{TimeZone, Utc};
    use std::process::Command;

    #[test]
    fn croner_dst_contract_child() {
        if std::env::var_os("SHELLBOY_SCHEDULER_DST_CHILD").is_none() {
            return;
        }
        let cron: croner::Cron = "30 1 * * *".parse().unwrap();
        let spring_start = Utc
            .timestamp_opt(1_774_745_100, 0)
            .single()
            .unwrap()
            .with_timezone(&chrono::Local);
        assert_eq!(spring_start.format("%H:%M %z").to_string(), "00:45 +0000");
        let spring_next = cron.find_next_occurrence(&spring_start, false).unwrap();
        assert_eq!(spring_next.timestamp(), 1_774_746_000);
        assert_eq!(spring_next.format("%H:%M %z").to_string(), "02:00 +0100");

        let fold_start = Utc
            .timestamp_opt(1_792_888_200, 0)
            .single()
            .unwrap()
            .with_timezone(&chrono::Local);
        assert_eq!(fold_start.format("%H:%M %z").to_string(), "01:30 +0100");
        let fold_next = cron.find_next_occurrence(&fold_start, false).unwrap();
        assert_eq!(fold_next.timestamp(), 1_792_978_200);
        assert_eq!(fold_next.format("%H:%M %z").to_string(), "01:30 +0000");
        let minute_cron: croner::Cron = "* * * * *".parse().unwrap();
        let mut cursor = fold_start;
        let mut repeated_minute_hits = 0;
        for _ in 0..60 {
            cursor = minute_cron.find_next_occurrence(&cursor, false).unwrap();
            if cursor.format("%H:%M %z").to_string() == "01:30 +0000" {
                repeated_minute_hits += 1;
            }
        }
        assert_eq!(repeated_minute_hits, 0);
    }

    #[test]
    fn croner_local_gap_and_fold_behavior_is_explicit() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "control::scheduler::dst_behavior_tests::croner_dst_contract_child",
            ])
            .env("TZ", "Europe/London")
            .env("SHELLBOY_SCHEDULER_DST_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "DST child failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(test)]
mod run_limit_tests {
    use super::*;
    use crate::{
        app::DataOwner,
        protocol::{ScheduleAddArgs, ShellState},
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_limit_uses_shell_handle_and_persists_timed_out_without_exit_code() {
        crate::test_watchdog::watchdog(
            "scheduled run limit cancels the shared shell and records the actual stop time",
            async {
                assert_eq!(MAX_RUN_DURATION, Duration::from_secs(60 * 60));
                let root = tempfile::tempdir().unwrap();
                let config_path = root.path().join("config.toml");
                std::fs::write(
                    &config_path,
                    "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n",
                )
                .unwrap();
                let owner = DataOwner::start(
                    Some(config_path),
                    Some(root.path().join("data")),
                    root.path().into(),
                )
                .await
                .unwrap();
                let control = Control::new(&owner, root.path().into());
                let created = control
                    .store
                    .execute(
                        crate::protocol::Request::new(
                            format!("{}:run-limit-thread", control.store.instance_id),
                            crate::protocol::Command::ThreadCreate(
                                crate::protocol::ThreadCreateArgs {
                                    agents: None,
                                    connection: None,
                                    cwd: Some(root.path().to_string_lossy().into_owned()),
                                    model: None,
                                    effort: None,
                                    parent_id: None,
                                },
                            ),
                        ),
                        None,
                    )
                    .await
                    .unwrap();
                let thread_id = created.result["thread_id"].as_str().unwrap().to_owned();
                let future_at = chrono::Utc::now().timestamp() + 3600;
                let added = control
                    .store
                    .execute(
                        crate::protocol::Request::new(
                            format!("{}:run-limit-schedule", control.store.instance_id),
                            crate::protocol::Command::ScheduleAdd(ScheduleAddArgs {
                                thread_id: Some(thread_id.clone()),
                                command: "while true; do sleep 1; done".into(),
                                cwd: None,
                                cron: None,
                                at: Some(future_at),
                            }),
                        ),
                        None,
                    )
                    .await
                    .unwrap();
                let schedule_id = uuid::Uuid::parse_str(
                    added.result["schedule_id"].as_str().unwrap(),
                )
                .unwrap();
                let due_at = chrono::Utc::now().timestamp() - 1;
                let id_for_db = schedule_id.to_string();
                owner
                    .db
                    .call(move |db| {
                        db.execute(
                            "UPDATE scheduled_command SET next_at=?1 WHERE id=?2",
                            rusqlite::params![due_at, id_for_db],
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
                let due = crate::storage::schedule::advance_due(&owner.db, chrono::Utc::now().timestamp())
                    .await
                    .unwrap()
                    .unwrap();
                let before = chrono::Utc::now().timestamp();
                let execution = control
                    .shell
                    .spawn(
                        due.id.to_string(),
                        PathBuf::from(due.cwd),
                        ShellToolArgs {
                            command: due.command,
                            description: None,
                            background: false,
                            stdin_open: false,
                            initial_stdin: None,
                            input_encoding: "utf-8".into(),
                            output_encoding: "utf-8".into(),
                        },
                        [("SHELLBOY_THREAD_ID".to_string(), due.thread_id)]
                            .into_iter()
                            .collect(),
                    )
                    .await
                    .unwrap();
                let collected = collect_shell_output(
                    execution,
                    CancellationToken::new(),
                    Duration::from_millis(25),
                )
                .await
                .unwrap();
                assert!(collected.timed_out);
                assert_eq!(collected.completion.state, ShellState::Stopped);
                assert!(collected.completion.group_cleanup_confirmed);
                let result = scheduled_shell_result(due.scheduled_at, collected);
                assert_eq!(result.status, "timed_out");
                assert_eq!(result.exit_code, None);
                assert!(result.finished_at >= before);
                assert!(result.finished_at <= chrono::Utc::now().timestamp());
                crate::storage::schedule::finish(&owner.db, schedule_id, result)
                    .await
                    .unwrap();
                let rows = crate::storage::schedule::list(&owner.db, thread_id)
                    .await
                    .unwrap();
                assert_eq!(rows[0].result.as_ref().unwrap().status, "timed_out");
                assert_eq!(rows[0].result.as_ref().unwrap().exit_code, None);
                assert!(rows[0].result.as_ref().unwrap().finished_at >= before);
                control.shutdown().await.unwrap();
                owner.shutdown().await.unwrap();
            },
        )
        .await
        .unwrap();
    }
}
