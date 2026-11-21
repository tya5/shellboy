use super::Control;
use crate::{
    control::db::MutationResult,
    protocol::{ApiError, Command, ErrorCode, Origin, Request, ShellToolArgs},
    shell::{ShellCompletion, ShellEvent, ShellExecution},
    storage::schedule::{self, DueSchedule, ScheduledCommandResult},
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Weak,
};
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::{Duration, sleep},
};
use tokio_util::sync::CancellationToken;

const OUTPUT_TAIL_BYTES: usize = 4096;
const MAX_SLEEP_SECONDS: u64 = 60;
const MAX_RUN_DURATION: Duration = Duration::from_secs(60 * 60);
/// One outstanding scheduler-sensitive request can wait behind the owner loop.
pub(super) const REQUEST_CAPACITY: usize = 1;

pub(super) struct Worker {
    pub requests: mpsc::Sender<SchedulerRequest>,
    pub task: Mutex<Option<JoinHandle<()>>>,
}

pub(super) enum SchedulerRequest {
    ScheduleRemove {
        request: Request,
        origin: Option<Origin>,
        reply: oneshot::Sender<Result<MutationResult, ApiError>>,
    },
    ThreadDelete {
        request: Request,
        origin: Option<Origin>,
        reply: oneshot::Sender<Result<MutationResult, ApiError>>,
    },
    ThreadDeletePreview {
        request: Request,
        origin: Option<Origin>,
        reply: oneshot::Sender<Result<(MutationResult, String), ApiError>>,
    },
}

fn next_sleep_seconds(next_at: i64, now: i64) -> Option<u64> {
    let remaining = next_at.saturating_sub(now);
    (remaining > 0).then(|| remaining.min(MAX_SLEEP_SECONDS as i64) as u64)
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

struct RunOutcome {
    schedule_id: uuid::Uuid,
    result: Result<ScheduledCommandResult, ApiError>,
}

pub(super) async fn run(
    control: Weak<Control>,
    stop: CancellationToken,
    mut requests: mpsc::Receiver<SchedulerRequest>,
) {
    let child_stop = stop.child_token();
    let mut children = JoinSet::new();
    let mut running = HashMap::<uuid::Uuid, String>::new();
    let mut pending = HashMap::<uuid::Uuid, DueSchedule>::new();
    let mut failure = None;

    'dispatcher: loop {
        let Some(owner) = control.upgrade() else {
            break;
        };
        if stop.is_cancelled() {
            break;
        }

        // Retire finished children before reading blocker snapshots. Then
        // channel requests are linearized before any pending launch.
        while let Some(joined) = children.try_join_next() {
            match finish_child(&owner, joined, &mut running).await {
                Ok(()) => {}
                Err(error) => {
                    failure = Some(error);
                    break 'dispatcher;
                }
            }
        }
        while let Ok(request) = requests.try_recv() {
            if let Some(error) = handle_request(&owner, request, &mut running, &mut pending).await {
                failure = Some(error);
                break 'dispatcher;
            }
        }

        // A normal Thread edit can temporarily hold the existing admission
        // boundary. Keep that due row in this scheduler-owned map and retry on
        // the request-completion notification; no shared registry or DB claim.
        let mut processed_pending = false;
        let pending_ids = pending.keys().copied().collect::<Vec<_>>();
        for id in pending_ids {
            let Some(due) = pending.get(&id) else {
                continue;
            };
            let boundary = owner.boundary(&due.thread_id).await;
            let Ok(admission) = boundary.try_lock_owned() else {
                continue;
            };
            let due = pending.remove(&id).expect("pending ID remains present");
            let thread_id = due.thread_id.clone();
            let execution = owner
                .shell
                .spawn(
                    id.to_string(),
                    PathBuf::from(due.cwd.clone()),
                    ShellToolArgs {
                        command: due.command.clone(),
                        description: None,
                        background: false,
                        stdin_open: false,
                        initial_stdin: None,
                        input_encoding: "utf-8".into(),
                        output_encoding: "utf-8".into(),
                    },
                    [("SHELLBOY_THREAD_ID".to_string(), thread_id.clone())]
                        .into_iter()
                        .collect(),
                )
                .await;
            drop(admission);
            match execution {
                Ok(execution) => {
                    running.insert(id, thread_id.clone());
                    let stop = child_stop.clone();
                    children.spawn(async move { run_shell(due, execution, stop).await });
                }
                Err(error) => {
                    let result = ScheduledCommandResult {
                        scheduled_at: due.scheduled_at,
                        finished_at: chrono::Utc::now().timestamp(),
                        status: "launch_failed".into(),
                        exit_code: None,
                        output_tail: Some(format!("Scheduler: launch failed: {error}")),
                        output_truncated: false,
                    };
                    if let Err(storage) = schedule::finish(&owner.store.database, id, result).await
                    {
                        failure = Some(storage.into_api_error());
                        break 'dispatcher;
                    }
                }
            }
            processed_pending = true;
            break;
        }
        if processed_pending {
            continue;
        }

        let changed = owner.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();

        let deadline = match schedule::next_deadline(&owner.store.database).await {
            Ok(deadline) => deadline,
            Err(storage) => {
                failure = Some(storage.into_api_error());
                break;
            }
        };
        let now = chrono::Utc::now().timestamp();
        if deadline.is_some_and(|at| at <= now) {
            let due = match schedule::advance_due(&owner.store.database, now).await {
                Ok(due) => due,
                Err(storage) => {
                    failure = Some(storage.into_api_error());
                    break;
                }
            };
            if let Some(due) = due {
                let id = due.id;
                if running.contains_key(&id) || pending.contains_key(&id) {
                    let result = ScheduledCommandResult {
                        scheduled_at: due.scheduled_at,
                        finished_at: chrono::Utc::now().timestamp(),
                        status: "overlap_skipped".into(),
                        exit_code: None,
                        output_tail: None,
                        output_truncated: false,
                    };
                    if let Err(storage) = schedule::finish(&owner.store.database, id, result).await
                    {
                        failure = Some(storage.into_api_error());
                        break;
                    }
                } else {
                    pending.insert(id, due);
                }
            }
            continue;
        }

        let sleep_seconds = deadline.and_then(|at| next_sleep_seconds(at, now));
        tokio::select! { biased;
            _ = stop.cancelled() => break 'dispatcher,
            joined = children.join_next(), if !children.is_empty() => {
                if let Some(joined) = joined
                    && let Err(error) = finish_child(&owner, joined, &mut running).await
                {
                    failure = Some(error);
                    break 'dispatcher;
                }
            },
            request = requests.recv() => match request {
                Some(request) => {
                    if let Some(error) = handle_request(&owner, request, &mut running, &mut pending).await {
                        failure = Some(error);
                        break 'dispatcher;
                    }
                }
                None => break 'dispatcher,
            },
            _ = &mut changed => {},
            _ = sleep(Duration::from_secs(sleep_seconds.unwrap_or(0))), if sleep_seconds.is_some() => {},
        }
    }

    child_stop.cancel();
    while let Some(joined) = children.join_next().await {
        if let Some(owner) = control.upgrade()
            && let Err(error) = finish_child(&owner, joined, &mut running).await
        {
            failure.get_or_insert(error);
        }
    }
    if let (Some(owner), Some(error)) = (control.upgrade(), failure) {
        if error.is_storage_failure() {
            owner.fail_storage().await;
        } else {
            owner.publish_event(super::EngineEvent::Error {
                thread_id: String::new(),
                error,
            });
        }
    }
}

async fn handle_request(
    owner: &Control,
    request: SchedulerRequest,
    running: &HashMap<uuid::Uuid, String>,
    pending: &mut HashMap<uuid::Uuid, DueSchedule>,
) -> Option<ApiError> {
    match request {
        SchedulerRequest::ScheduleRemove {
            request,
            origin,
            reply,
        } => {
            let schedule_id = match &request.command {
                Command::ScheduleRemove(args) => uuid::Uuid::parse_str(&args.schedule_id).ok(),
                _ => None,
            };
            let result = owner.store.execute(request, origin).await;
            if let (Ok(result), Some(schedule_id)) = (&result, schedule_id)
                && result.result["removed"] == true
            {
                pending.remove(&schedule_id);
                owner.changed.notify_waiters();
            }
            let storage_failure = result
                .as_ref()
                .err()
                .filter(|error| error.is_storage_failure())
                .cloned();
            let _ = reply.send(result);
            storage_failure
        }
        SchedulerRequest::ThreadDeletePreview {
            request,
            origin,
            reply,
        } => {
            let active_threads = running.values().cloned().collect();
            let result = owner
                .store
                .execute_with_active_delete_preview(request, origin, active_threads)
                .await;
            let storage_failure = result
                .as_ref()
                .err()
                .filter(|error| error.is_storage_failure())
                .cloned();
            let _ = reply.send(result);
            storage_failure
        }
        SchedulerRequest::ThreadDelete {
            request,
            origin,
            reply,
        } => {
            let result = delete_thread(owner, request, origin, running).await;
            if let Ok((effects, deleted_targets)) = &result {
                let deleted = deleted_targets
                    .iter()
                    .collect::<std::collections::HashSet<_>>();
                pending.retain(|_, due| !deleted.contains(&due.thread_id));
                if !effects.deleted_threads.is_empty() {
                    owner.changed.notify_waiters();
                }
            }
            let result = result.map(|(effects, _)| effects);
            let storage_failure = result
                .as_ref()
                .err()
                .filter(|error| error.is_storage_failure())
                .cloned();
            let _ = reply.send(result);
            storage_failure
        }
    }
}

async fn delete_thread(
    owner: &Control,
    request: Request,
    origin: Option<Origin>,
    running: &HashMap<uuid::Uuid, String>,
) -> Result<(MutationResult, Vec<String>), ApiError> {
    if let Some(saved) = owner.store.replay(&request, &origin).await? {
        return Ok((saved, Vec::new()));
    }
    let Command::ThreadDelete(args) = &request.command else {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "ThreadDelete request required",
        ));
    };
    let selection = args.clone();
    let checked_targets = owner
        .store
        .database
        .call(move |db| {
            if selection.preview_marker.is_some() {
                Ok(crate::deletion::guarded_targets(db, &selection)
                    .map_err(crate::storage::StorageError::from)?
                    .unwrap_or_default())
            } else {
                crate::deletion::actual_delete_targets(db, &selection)
            }
        })
        .await
        .map_err(super::storage_error)?;
    let mut guards = Vec::new();
    for thread in &checked_targets {
        let boundary = owner.boundary(thread).await;
        guards.push(boundary.try_lock_owned().map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "A Thread-owned edit is active; retry deletion after it ends",
            )
        })?);
    }
    if checked_targets
        .iter()
        .any(|thread| running.values().any(|active| active == thread))
    {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "A scheduled shell is active for this Thread; wait until it finishes before deleting",
        ));
    }
    let result = owner
        .store
        .execute_with_checked_delete_targets(request, origin, checked_targets.clone())
        .await?;
    Ok((result, checked_targets))
}

async fn finish_child(
    owner: &Control,
    joined: Result<RunOutcome, tokio::task::JoinError>,
    running: &mut HashMap<uuid::Uuid, String>,
) -> Result<(), ApiError> {
    let outcome = joined.map_err(|error| {
        ApiError::new(
            ErrorCode::InvalidState,
            format!("scheduled shell task failed: {error}"),
        )
    })?;
    running.remove(&outcome.schedule_id);
    let result = outcome.result?;
    schedule::finish(&owner.store.database, outcome.schedule_id, result)
        .await
        .map_err(crate::app::storage_error)
}

async fn run_shell(
    due: DueSchedule,
    execution: ShellExecution,
    stop: CancellationToken,
) -> RunOutcome {
    let id = due.id;
    let result = collect_shell_output(execution, stop, MAX_RUN_DURATION)
        .await
        .map(|collected| scheduled_shell_result(due.scheduled_at, collected));
    RunOutcome {
        schedule_id: id,
        result,
    }
}

struct CollectedShell {
    completion: ShellCompletion,
    output_tail: Option<String>,
    output_truncated: bool,
    timed_out: bool,
    finished_at: i64,
}

async fn wait_for_completion(
    completion: &mut tokio::sync::watch::Receiver<Option<ShellCompletion>>,
) -> Result<ShellCompletion, ApiError> {
    loop {
        if let Some(value) = completion.borrow().clone() {
            return Ok(value);
        }
        completion.changed().await.map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "scheduled shell completion was unavailable",
            )
        })?;
    }
}

async fn drain_shell_output(
    mut events: mpsc::Receiver<ShellEvent>,
) -> (Option<String>, bool, bool) {
    let mut tail = VecDeque::with_capacity(OUTPUT_TAIL_BYTES);
    let mut truncated = false;
    while let Some(event) = events.recv().await {
        match event {
            ShellEvent::Output(chunk) => {
                for byte in chunk.bytes.iter().copied() {
                    if tail.len() == OUTPUT_TAIL_BYTES {
                        tail.pop_front();
                        truncated = true;
                    }
                    tail.push_back(byte);
                }
            }
            ShellEvent::Stdin { .. } => {}
        }
    }
    let mut output = String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned();
    while output.len() > OUTPUT_TAIL_BYTES {
        output.remove(0);
        truncated = true;
    }
    (!output.is_empty())
        .then_some(output)
        .map_or((None, truncated, false), |value| {
            (Some(value), truncated, true)
        })
}

async fn collect_shell_output(
    execution: ShellExecution,
    stop: CancellationToken,
    run_limit: Duration,
) -> Result<CollectedShell, ApiError> {
    let ShellExecution {
        handle,
        events,
        completion: mut completion_rx,
    } = execution;
    let completion = async {
        tokio::select! { biased;
            _ = stop.cancelled() => {
                let stop_at = chrono::Utc::now().timestamp();
                handle.stop();
                let completion = wait_for_completion(&mut completion_rx).await?;
                Ok::<_, ApiError>((completion, false, stop_at))
            }
            result = tokio::time::timeout(run_limit, wait_for_completion(&mut completion_rx)) => {
                match result {
                    Ok(completion) => Ok((completion?, false, chrono::Utc::now().timestamp())),
                    Err(_) => {
                        let stop_at = chrono::Utc::now().timestamp();
                        handle.stop();
                        let completion = wait_for_completion(&mut completion_rx).await?;
                        Ok((completion, true, stop_at))
                    }
                }
            }
        }
    };
    let (completion, output) = tokio::join!(completion, drain_shell_output(events));
    let (completion, timed_out, stopped_at) = completion?;
    let (output_tail, output_truncated, _) = output;
    Ok(CollectedShell {
        output_tail,
        output_truncated: output_truncated || !completion.output_complete,
        finished_at: stopped_at,
        timed_out,
        completion,
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
                                command: "while true; do printf x; done".into(),
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
                    Duration::from_millis(250),
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
