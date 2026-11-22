//! Advance a scheduled occurrence and reserve its ordinary shell in one transaction.
use super::{
    Control, ShellToolArgs,
    driver::shell_output,
    runtime::{ShellStopIntent, record_shell_stop},
};
use crate::protocol::ApiError;
use crate::storage::StorageError;
use chrono::{DateTime, Local, TimeZone, Utc};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{path::PathBuf, sync::Arc};

struct DueShell {
    id: String,
    thread: String,
    cwd: PathBuf,
    command: String,
}

fn next_cron(expression: &str, now: DateTime<Utc>, zone: &impl TimeZone) -> Option<i64> {
    if expression.split_whitespace().count() != 5 {
        return None;
    }
    expression
        .parse::<croner::Cron>()
        .ok()?
        .find_next_occurrence(&now.with_timezone(zone), false)
        .ok()
        .map(|next| next.timestamp())
}

#[derive(Debug, PartialEq, Eq)]
enum Occurrence {
    NotDue,
    Due { next: Option<i64>, invalid: bool },
}

fn occurrence(
    next_at: i64,
    cron: Option<&str>,
    now: DateTime<Utc>,
    zone: &impl TimeZone,
) -> Occurrence {
    if next_at > now.timestamp() {
        return Occurrence::NotDue;
    }
    let next = cron.and_then(|expression| next_cron(expression, now, zone));
    Occurrence::Due {
        next,
        invalid: cron.is_some() && next.is_none(),
    }
}

fn advance_due(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    shell_kind: &str,
) -> Result<Option<DueShell>, StorageError> {
    let due = tx.query_row(
        "SELECT id,thread_id,cwd,command,cron,next_at FROM schedule WHERE enabled=1 AND next_at IS NOT NULL ORDER BY next_at,id LIMIT 1", [],
        |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?)),
    ).optional()?;
    let Some((schedule, thread, cwd, command, cron, scheduled_at)) = due else {
        return Ok(None);
    };
    let Occurrence::Due { next, invalid } = occurrence(scheduled_at, cron.as_deref(), now, &Local)
    else {
        return Ok(None);
    };
    let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE schedule_id=?1 AND state IN ('starting','running','stopping'))",[&schedule],|r| r.get(0))?;
    tx.execute("UPDATE schedule SET next_at=?2,last_skipped_at=CASE WHEN ?3 THEN ?4 ELSE last_skipped_at END WHERE id=?1",params![schedule,next,active,scheduled_at])?;
    if active && !invalid {
        return Ok(None);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let shell = serde_json::json!({"command": command,"kind":shell_kind});
    let at = now.to_rfc3339();
    tx.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,end_reason,output_complete) VALUES(?1,?2,?1,?3,?4,?5,'utf-8','utf-8','closed',?6,?7,?8,?9)",params![id,schedule,if invalid {"failed"} else {"starting"},cwd,shell.to_string(),at,invalid.then_some(&at),invalid.then_some("schedule_recurrence_calculation_failed"),invalid.then_some(false)])?;
    Ok((!invalid).then(|| DueShell {
        id,
        thread,
        cwd: cwd.into(),
        command,
    }))
}

fn read_deadline(db: &rusqlite::Connection) -> rusqlite::Result<Option<DateTime<Utc>>> {
    db.query_row(
        "SELECT min(deadline) FROM (SELECT min(next_at) AS deadline FROM schedule WHERE enabled=1 AND next_at IS NOT NULL UNION ALL SELECT min(unixepoch(started_at)+3600) FROM shell_execution WHERE schedule_id IS NOT NULL AND state IN ('starting','running'))",
        [], |r| r.get::<_, Option<i64>>(0),
    ).map(|next|next.and_then(|at|DateTime::from_timestamp(at,0)))
}

pub(super) async fn next_deadline(
    control: &Control,
) -> Result<Option<DateTime<Utc>>, StorageError> {
    control
        .store
        .database
        .call(|db| Ok(read_deadline(db)?))
        .await
}

pub(super) async fn dispatch(control: &Arc<Control>) -> Result<Option<DateTime<Utc>>, ApiError> {
    let kind = serde_json::to_value(control.store.config.shell.kind).unwrap();
    let (shell, next, expired) = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let now = Utc::now();
            let expired: Option<String> = tx.query_row("SELECT id FROM shell_execution WHERE schedule_id IS NOT NULL AND state IN ('starting','running') AND unixepoch(started_at)<=?1 ORDER BY started_at,id LIMIT 1",[now.timestamp()-3600],|r|r.get(0)).optional()?;
            if let Some(id) = &expired {
                record_shell_stop(&tx,id,ShellStopIntent::ScheduleTimeout)?;
            }
            let shell = advance_due(&tx, now, kind.as_str().unwrap_or_default())?;
            let next = read_deadline(&tx)?;
            tx.commit()?;
            Ok((
                shell,
                next,
                expired,
            ))
        })
        .await
        .map_err(super::storage_error)?;
    if let Some(id) = expired
        && let Some(handle) = control.live.clone_shell_handle(&id).await
    {
        handle.stop();
    }
    if let Some(shell) = shell {
        if control.shell_start_intent(&shell.id, false).await?.0 {
            control.changed(&shell.thread);
            return Ok(next);
        }
        let args = ShellToolArgs {
            command: shell.command,
            description: None,
            background: true,
            stdin_open: false,
            initial_stdin: None,
            input_encoding: "utf-8".into(),
            output_encoding: "utf-8".into(),
        };
        let env = shell_output::cli_environment(control, &shell.thread);
        match control
            .shell
            .spawn(shell.id.clone(), shell.cwd, args, env)
            .await
        {
            Ok(spawned) => {
                shell_output::install_shell(
                    control,
                    shell_output::ShellOwner::Schedule {
                        thread: shell.thread,
                    },
                    None,
                    &shell.id,
                    spawned,
                )
                .await?;
            }
            Err(_) => {
                control
                    .store
                    .database
                    .call(move |db| {
                        let tx = db.transaction()?;
                        if super::state::fail_shell_spawn(&tx, &shell.id, &super::state::now())?
                            != 0
                        {
                            retire_oneshot(&tx, &shell.id)?;
                        }
                        tx.commit()?;
                        Ok(())
                    })
                    .await
                    .map_err(super::storage_error)?;
                control.changed(&shell.thread);
            }
        }
    }
    Ok(next)
}

// Called only for an execution just settled in this transaction. A preexisting
// spent one-shot is not a retirement candidate on startup or migration.
pub(super) fn retire_oneshot(tx: &Transaction<'_>, execution: &str) -> rusqlite::Result<()> {
    let owner: Option<(String, String)> = tx.query_row(
        "SELECT q.id,q.thread_id FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE s.id=?1 AND s.state IN ('exited','failed','stopped') AND q.once_at IS NOT NULL AND q.next_at IS NULL",
        [execution], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    if let Some((schedule, thread)) = owner {
        tx.execute(
            "UPDATE shell_execution SET schedule_id=NULL,detached_thread_id=?2 WHERE id=?1",
            params![execution, thread],
        )?;
        tx.execute("DELETE FROM schedule WHERE id=?1", [schedule])?;
    }
    Ok(())
}

fn invalid(message: &'static str) -> ApiError {
    ApiError::new(crate::protocol::ErrorCode::InvalidArgument, message)
}

pub(super) fn add(
    tx: &Transaction<'_>,
    thread: &str,
    a: &crate::protocol::ScheduleAddArgs,
) -> Result<serde_json::Value, ApiError> {
    let sql = |e| super::storage_error(StorageError::from(e));
    let cwd: String = tx
        .query_row(
            "SELECT cwd FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [thread],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?
        .ok_or_else(|| {
            ApiError::new(
                crate::protocol::ErrorCode::TargetUnavailable,
                "Thread not found",
            )
        })?;
    if a.command.is_empty() || a.command.len() > 32 * 1024 || a.command.contains('\0') {
        return Err(invalid(
            "Schedule command must be 1 to 32768 bytes without NUL",
        ));
    }
    if a.description
        .as_ref()
        .is_some_and(|s| s.chars().count() > crate::tool_description::MAX_CHARS)
    {
        return Err(invalid(
            "Schedule description exceeds the description limit",
        ));
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM schedule WHERE thread_id=?1",
            [thread],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if count >= 64 {
        return Err(invalid("A Thread may have at most 64 schedules"));
    }
    let now = Utc::now();
    let (once, next) = match (&a.cron, &a.at, a.in_seconds) {
        (Some(cron), None, None) => (None, next_cron(cron, now, &Local)),
        (None, Some(at), None) => {
            let at = DateTime::parse_from_rfc3339(at).ok().map(|v| v.timestamp());
            (at, at)
        }
        (None, None, Some(seconds)) => {
            let at = i64::try_from(seconds)
                .ok()
                .filter(|s| *s > 0)
                .and_then(chrono::TimeDelta::try_seconds)
                .and_then(|delta| now.checked_add_signed(delta))
                .map(|at| at.timestamp());
            (at, at)
        }
        _ => return Err(invalid("Specify exactly one schedule time")),
    };
    let next = next.ok_or_else(|| invalid("Cannot calculate the schedule time"))?;
    let cwd = PathBuf::from(cwd).join(a.cwd.as_deref().unwrap_or(""));
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT INTO schedule(id,thread_id,cron,once_at,next_at,enabled,cwd,command,description) VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8)",params![id,thread,a.cron,once,next,cwd.to_string_lossy(),a.command,a.description]).map_err(sql)?;
    Ok(
        serde_json::json!({"schedule_id":id,"thread_id":thread,"next_at":DateTime::from_timestamp(next,0).map(|at|at.to_rfc3339())}),
    )
}

pub(super) fn list(tx: &Transaction<'_>, thread: &str) -> Result<serde_json::Value, ApiError> {
    let sql = |e| super::storage_error(StorageError::from(e));
    let mut query=tx.prepare("SELECT q.id,q.enabled,q.next_at,q.cron,q.once_at,q.cwd,q.command,q.description,q.last_skipped_at,s.id,s.state,s.exit_code,s.end_reason,s.started_at,s.ended_at FROM schedule q JOIN thread t ON t.id=q.thread_id AND t.deleted_at IS NULL LEFT JOIN shell_execution s ON s.id=(SELECT id FROM shell_execution WHERE schedule_id=q.id ORDER BY started_at DESC,id DESC LIMIT 1) WHERE q.thread_id=?1 ORDER BY q.id LIMIT 64").map_err(sql)?;
    let items=query.query_map([thread],|r| {
        let id:Option<String>=r.get(9)?;
        let latest=if let Some(id)=id {
            let mut remaining=4096;
            let mut tail=Vec::new();
            let mut chunks=tx.prepare("SELECT substr(bytes,max(1,length(bytes)-?2+1)) FROM output_chunk WHERE execution_id=?1 ORDER BY sequence DESC LIMIT 4096")?;
            let mut rows=chunks.query(params![id,remaining as i64])?;
            while remaining>0 {let Some(row)=rows.next()? else {break}; let bytes:Vec<u8>=row.get(0)?; let n=remaining.min(bytes.len()); tail.extend(bytes[bytes.len()-n..].iter().rev());remaining-=n;}
            tail.reverse();
            serde_json::json!({"execution_id":id,"state":r.get::<_,String>(10)?,"exit_code":r.get::<_,Option<i64>>(11)?,"end_reason":r.get::<_,Option<String>>(12)?,"started_at":r.get::<_,String>(13)?,"ended_at":r.get::<_,Option<String>>(14)?,"output_tail":String::from_utf8_lossy(&tail)})
        }else{serde_json::Value::Null};
        let timestamp=|i| r.get::<_,Option<i64>>(i).map(|v|v.and_then(|at|DateTime::from_timestamp(at,0)).map(|at|at.to_rfc3339()));
        Ok(serde_json::json!({"schedule_id":r.get::<_,String>(0)?,"enabled":r.get::<_,bool>(1)?,"next_at":timestamp(2)?,"cron":r.get::<_,Option<String>>(3)?,"at":timestamp(4)?,"cwd":r.get::<_,String>(5)?,"command":r.get::<_,String>(6)?,"description":r.get::<_,Option<String>>(7)?,"last_skipped_at":timestamp(8)?,"latest":latest}))
    }).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    Ok(serde_json::json!({"thread_id":thread,"items":items}))
}

pub(super) enum Change<'a> {
    Description(&'a str),
    Enabled(bool),
}

pub(super) fn change(
    tx: &Transaction<'_>,
    id: &str,
    change: Change<'_>,
) -> Result<super::MutationResult, ApiError> {
    let sql = |e| super::storage_error(StorageError::from(e));
    let (thread, cron, once, enabled): (String, Option<String>, Option<i64>, bool) = tx
        .query_row(
            "SELECT thread_id,cron,once_at,enabled FROM schedule WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sql)?
        .ok_or_else(|| {
            ApiError::new(
                crate::protocol::ErrorCode::TargetUnavailable,
                "Schedule not found",
            )
        })?;
    match change {
        Change::Description(text) => {
            if text.chars().count() > crate::tool_description::MAX_CHARS {
                return Err(invalid(
                    "Schedule description exceeds the description limit",
                ));
            }
            tx.execute(
                "UPDATE schedule SET description=NULLIF(?2,'') WHERE id=?1",
                params![id, text],
            )
            .map_err(sql)?;
        }
        Change::Enabled(next_enabled) if next_enabled != enabled => {
            let now = Utc::now();
            let next = if next_enabled {
                match cron {
                    Some(cron) => Some(
                        next_cron(&cron, now, &Local)
                            .ok_or_else(|| invalid("Cannot calculate the schedule time"))?,
                    ),
                    None => once.filter(|value| *value > now.timestamp()),
                }
            } else {
                None
            };
            tx.execute(
                "UPDATE schedule SET enabled=?2,next_at=?3 WHERE id=?1",
                params![id, next_enabled, next],
            )
            .map_err(sql)?;
        }
        Change::Enabled(_) => {}
    }
    let mut result = super::MutationResult::value(serde_json::json!({"schedule_id":id}))?;
    result.changed_threads.push(thread);
    Ok(result)
}

fn disable_and_stop(
    tx: &Transaction<'_>,
    key: &str,
    by_thread: bool,
) -> Result<Vec<String>, StorageError> {
    let column = if by_thread { "thread_id" } else { "id" };
    tx.execute(
        &format!("UPDATE schedule SET enabled=0 WHERE {column}=?1"),
        [key],
    )?;
    let ids=tx.prepare(&format!("SELECT s.id FROM schedule q JOIN shell_execution s ON s.schedule_id=q.id WHERE q.{column}=?1 AND s.state IN ('starting','running','stopping')"))?.query_map([key],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let intent = if by_thread {
        ShellStopIntent::ThreadDeletion
    } else {
        ShellStopIntent::ScheduleDeletion
    };
    for id in &ids {
        record_shell_stop(tx, id, intent)?;
    }
    Ok(ids)
}

pub(super) fn ensure_inactive(
    tx: &Transaction<'_>,
    key: &str,
    thread: bool,
) -> Result<(), ApiError> {
    let column = if thread { "thread_id" } else { "id" };
    let active=tx.query_row(&format!("SELECT s.id,q.id FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE q.{column}=?1 AND s.state IN ('starting','running','stopping') ORDER BY s.id LIMIT 1"),[key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(StorageError::from).map_err(super::storage_error)?;
    if let Some((execution, schedule)) = active {
        let mut error = ApiError::new(
            crate::protocol::ErrorCode::InvalidState,
            "A scheduled execution is active",
        );
        error.details = Some(
            serde_json::json!({"reason":"active_scheduled_execution","target_id":key,"schedule_id":schedule,"execution_id":execution}),
        );
        return Err(error);
    }
    Ok(())
}

pub(super) async fn delete(
    control: &Arc<Control>,
    request: crate::protocol::Request,
) -> Result<serde_json::Value, ApiError> {
    use crate::protocol::{Command, ErrorCode};
    let command = request.command;
    let prepare = command.clone();
    let prepared = async {
        let (targets, executions) = control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let (mut targets, by_thread) = match prepare {
                    Command::ThreadDelete(args) => (super::db::delete_targets(&tx, &args)?, true),
                    Command::ScheduleRemove(args) => {
                        let exists = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM schedule WHERE id=?1)",
                            [&args.schedule_id],
                            |r| r.get::<_, bool>(0),
                        )?;
                        if !exists {
                            return Err(ApiError::new(
                                ErrorCode::TargetUnavailable,
                                "Schedule not found",
                            )
                            .into());
                        }
                        (vec![args.schedule_id], false)
                    }
                    _ => unreachable!("only deletion commands have this route"),
                };
                targets.sort_unstable();
                let mut executions = Vec::new();
                for target in &targets {
                    executions.extend(disable_and_stop(&tx, target, by_thread)?);
                }
                tx.commit()?;
                Ok((targets, executions))
            })
            .await
            .map_err(super::storage_error)?;
        drain(control, &executions).await?;
        Ok::<_, ApiError>((targets, executions))
    }
    .await;
    if prepared.as_ref().is_err_and(ApiError::is_storage_failure) {
        return prepared.map(|_| unreachable!());
    }
    let effects = control
        .store
        .finalize_deletion(request.operation_id, move |tx| {
            let (targets, executions) = prepared?;
            match command {
                Command::ThreadDelete(args) => {
                    let mut current = super::db::delete_targets(tx, &args)?;
                    current.sort_unstable();
                    if current != targets {
                        return Err(ApiError::new(
                            ErrorCode::PreviewStale,
                            "Deletion target set changed",
                        ));
                    }
                    super::db::delete_thread(tx, &args, &super::state::now())
                }
                Command::ScheduleRemove(args) => {
                    let id = args.schedule_id;
                    ensure_inactive(tx, &id, false)?;
                    let thread = tx.query_row(
                        "DELETE FROM schedule WHERE id=?1 RETURNING thread_id", [&id], |r| r.get::<_, String>(0),
                    ).optional().map_err(StorageError::from).map_err(super::storage_error)?;
                    let thread = match thread {
                        Some(thread) => thread,
                        None => tx.query_row(
                            "SELECT s.detached_thread_id FROM json_each(?1) j CROSS JOIN shell_execution s ON s.id=j.value WHERE s.detached_thread_id IS NOT NULL LIMIT 1",
                            [serde_json::json!(executions).to_string()], |r| r.get::<_,String>(0),
                        ).optional().map_err(StorageError::from).map_err(super::storage_error)?.ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable,"Schedule not found"))?,
                    };
                    let mut result = super::MutationResult::value(
                        serde_json::json!({"accepted":true,"schedule_id":id}),
                    )?;
                    result.changed_threads.push(thread);
                    Ok(result)
                }
                _ => unreachable!("only deletion commands have this route"),
            }
        })
        .await?;
    let signals = control.store.database.signals();
    if !effects.deleted_threads.is_empty() {
        signals.mark_all(&crate::maintenance::Work::DELETION);
        signals.mark_all(&[
            crate::maintenance::Work::Managed,
            crate::maintenance::Work::Drafts,
        ]);
    }
    signals.mark(crate::maintenance::Work::Schedule);
    let value = effects.result.clone();
    control.apply_effects(Some(effects)).await;
    Ok(value)
}

async fn drain(control: &Control, executions: &[String]) -> Result<(), ApiError> {
    if executions.is_empty() {
        return Ok(());
    }
    for id in executions {
        if let Some(handle) = control.live.clone_shell_handle(id).await {
            handle.stop();
        }
    }
    let targets = Arc::new(serde_json::json!(executions).to_string());
    loop {
        let changed = control.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let targets = targets.clone();
        let active = control.store.database.call(move |db| {
            Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM json_each(?1) j CROSS JOIN shell_execution s ON s.id=j.value WHERE s.state IN ('starting','running','stopping'))",[targets.as_str()],|r|r.get::<_,bool>(0))?)
        }).await.map_err(super::storage_error)?;
        if !active {
            return Ok(());
        }
        changed.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Command, Request};
    use serde_json::json;

    #[test]
    fn wall_clock_jumps_coalesce_missed_occurrences_without_replaying() {
        let at = |hour, minute| Utc.with_ymd_and_hms(2025, 1, 2, hour, minute, 0).unwrap();
        let stored = at(8, 0).timestamp();
        let cron = Some("0 * * * *");
        assert_eq!(
            occurrence(stored, cron, at(7, 59), &Utc),
            Occurrence::NotDue
        );
        let next = at(13, 0).timestamp();
        assert_eq!(
            occurrence(stored, cron, at(12, 20), &Utc),
            Occurrence::Due {
                next: Some(next),
                invalid: false
            }
        );
        assert_eq!(occurrence(next, cron, at(9, 0), &Utc), Occurrence::NotDue);
        assert_eq!(occurrence(next, cron, at(12, 20), &Utc), Occurrence::NotDue);
        assert_eq!(
            occurrence(next, cron, at(13, 0), &Utc),
            Occurrence::Due {
                next: Some(at(14, 0).timestamp()),
                invalid: false
            }
        );
    }

    #[test]
    fn croner_local_time_gap_and_fold() {
        use chrono_tz::Europe::Stockholm;
        let spring = Utc.with_ymd_and_hms(2025, 3, 30, 0, 59, 59).unwrap();
        let gap_end = Utc.with_ymd_and_hms(2025, 3, 30, 1, 0, 0).unwrap();
        assert_eq!(
            next_cron("30 2 * * *", spring, &Stockholm),
            Some(gap_end.timestamp())
        );
        assert_eq!(
            next_cron("*/5 * * * *", spring, &Stockholm),
            Some(gap_end.timestamp())
        );
        let fall = Utc.with_ymd_and_hms(2025, 10, 25, 23, 59, 59).unwrap();
        let first = Utc.with_ymd_and_hms(2025, 10, 26, 0, 30, 0).unwrap();
        let next_day = Utc.with_ymd_and_hms(2025, 10, 27, 1, 30, 0).unwrap();
        assert_eq!(
            next_cron("30 2 * * *", fall, &Stockholm),
            Some(first.timestamp())
        );
        assert_eq!(
            next_cron("30 2 * * *", first, &Stockholm),
            Some(next_day.timestamp())
        );
        let fold_edge = Utc.with_ymd_and_hms(2025, 10, 26, 0, 59, 0).unwrap();
        let second_hour = Utc.with_ymd_and_hms(2025, 10, 26, 1, 0, 0).unwrap();
        assert_eq!(
            next_cron("* * * * *", fold_edge, &Stockholm),
            Some(second_hour.timestamp())
        );
    }

    #[tokio::test]
    async fn deletion_reopen_observes_only_committed_boundaries() {
        use crate::protocol::{ErrorCode, ThreadDeleteArgs};
        for point in 0..4 {
            let root = tempfile::tempdir().unwrap();
            let data = root.path().join("data");
            let open =
                || crate::app::DataOwner::start(None, Some(data.clone()), root.path().into());
            let owner = open().await.unwrap();
            let control = Control::new(&owner, root.path().into());
            let args = ThreadDeleteArgs {
                thread_id: "target".into(),
                descendants: false,
                preview_marker: None,
            };
            let request = Request::new(
                format!("{}:delete", owner.instance_id),
                Command::ThreadDelete(args.clone()),
            );
            let cwd = root.path().to_string_lossy().into_owned();
            control.store.database.call(move|db| {
                db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('target',?1,'','fixture','fixture','now')",[&cwd])?;
                // Once claimed, the schedule and starting execution are committed
                // together, with next_at cleared by advance_due.
                db.execute("INSERT INTO schedule(id,thread_id,once_at,next_at,enabled,cwd,command) VALUES('q','target',4102444800,?2,1,?1,'echo fixture')",params![cwd,(point < 2).then_some(4102444800_i64)])?;
                if point >= 2 {
                    db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('x','q','x','starting',?1,'{}','utf-8','utf-8','closed','2020-01-01T00:00:00Z')",[cwd])?;
                }
                Ok(())
            }).await.unwrap();
            if point >= 1 {
                assert!(matches!(
                    control
                        .store
                        .reserve_runtime(request.clone(), None)
                        .await
                        .unwrap(),
                    super::super::db::RuntimeReservation::New
                ));
            }
            if point >= 2 {
                control
                    .store
                    .database
                    .call(|db| {
                        let tx = db.transaction()?;
                        assert_eq!(disable_and_stop(&tx, "target", true)?, ["x"]);
                        tx.commit()?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            let mut expected = None;
            if point == 3 {
                control.shell_start_intent("x", false).await.unwrap();
                let effects = control
                    .store
                    .finalize_deletion(request.operation_id.clone(), move |tx| {
                        super::super::db::delete_thread(tx, &args, &super::super::state::now())
                    })
                    .await
                    .unwrap();
                expected = Some(effects.result);
            }
            // Close the fixture without starting any workers. Reopen performs real startup repair.
            drop(control);
            owner.shutdown().await.unwrap();
            let owner = open().await.unwrap();
            let control = Control::new(&owner, root.path().into());
            let receipt = request.operation_id.clone();
            control.store.database.call(move|db| {
                let state=db.query_row("SELECT state FROM operation_receipt WHERE operation_id=?1",[receipt],|r|r.get::<_,String>(0)).optional()?;
                assert_eq!(state.as_deref(),[None,Some("pending"),Some("pending"),Some("completed")][point]);
                let enabled=db.query_row("SELECT enabled FROM schedule WHERE id='q'",[],|r|r.get::<_,bool>(0)).optional()?;
                assert_eq!(enabled,[Some(true),Some(true),None,None][point]);
                assert_eq!(db.query_row("SELECT deleted_at IS NOT NULL FROM thread WHERE id='target'",[],|r|r.get::<_,bool>(0))?,point==3);
                if point==2 {
                    let reason=db.query_row("SELECT end_reason FROM shell_execution WHERE id='x' AND state IN ('failed','stopped')",[],|r|r.get::<_,String>(0))?;
                    assert!(reason.starts_with("previous_instance_interrupted"));
                    assert_eq!(db.query_row("SELECT detached_thread_id FROM shell_execution WHERE id='x'",[],|r|r.get::<_,String>(0))?,"target");
                }
                if point==3 { assert!(!db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE id='x')",[],|r|r.get::<_,bool>(0))?); }
                Ok(())
            }).await.unwrap();
            let replay = control.request(request).await;
            match point {
                0 => assert_eq!(replay.error.unwrap().code, ErrorCode::OperationExpired),
                1 | 2 => assert_eq!(replay.error.unwrap().code, ErrorCode::InvalidState),
                _ => assert_eq!(replay.result, expected),
            }
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn schedule_drain_signals_both_handles_before_first_terminal_write() {
        let root = tempfile::tempdir().unwrap();
        let owner =
            crate::app::DataOwner::start(None, Some(root.path().join("data")), root.path().into())
                .await
                .unwrap();
        let control = Control::new(&owner, root.path().into());
        let cwd = root.path().to_str().unwrap().to_owned();
        owner.db.call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('target',?1,'','fixture','fixture','now')",[&cwd])?;
            for (schedule, id) in [("q1","first"),("q2","second")] {
                db.execute("INSERT INTO schedule(id,thread_id,once_at,enabled,cwd,command) VALUES(?1,'target',1,1,?2,'sleep 600')",params![schedule,cwd])?;
                db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES(?1,?2,?1,'running',?3,'{}','utf-8','utf-8','closed','2020-01-01T00:00:00Z')",params![id,schedule,cwd])?;
            }
            Ok(())
        }).await.unwrap();
        let mut shells = Vec::new();
        for id in ["first", "second"] {
            let shell = control
                .shell
                .spawn(
                    id.into(),
                    root.path().into(),
                    ShellToolArgs {
                        command: "exec sleep 600".into(),
                        description: None,
                        background: true,
                        stdin_open: false,
                        initial_stdin: None,
                        input_encoding: "utf-8".into(),
                        output_encoding: "utf-8".into(),
                    },
                    Default::default(),
                )
                .await
                .unwrap();
            control
                .live
                .register_shell(
                    id.into(),
                    super::super::LiveShell {
                        run_id: None,
                        handle: shell.handle.clone(),
                        last_committed_output: None,
                    },
                )
                .await;
            shells.push(shell);
        }
        let targets = owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                let mut ids = disable_and_stop(&tx, "target", true)?;
                ids.sort();
                tx.commit()?;
                Ok(ids)
            })
            .await
            .unwrap();
        let worker = control.clone();
        let draining = tokio::spawn(async move { drain(&worker, &targets).await });
        crate::test_watchdog::watchdog("both scheduled children stop before first terminal write",async {
            for shell in &mut shells {
                let completion=shell.completion.wait_for(Option::is_some).await.unwrap();
                assert_eq!(completion.as_ref().unwrap().state, crate::protocol::ShellState::Stopped);
            }
            // Deliver the second terminal write while the first collector stays held.
            owner.db.call(|db| {
                assert_eq!(db.execute("UPDATE shell_execution SET state='stopped' WHERE id='second' AND state='stopping'",[])?,1);
                assert_eq!(db.query_row("SELECT state FROM shell_execution WHERE id='first'",[],|r|r.get::<_,String>(0))?,"stopping");
                Ok(())
            }).await.unwrap();
            control.changed("target");
            assert!(!draining.is_finished());
            owner.db.call(|db| { db.execute("UPDATE shell_execution SET state='stopped' WHERE id='first'",[])?; Ok(()) }).await.unwrap();
            control.changed("target");
            draining.await.unwrap().unwrap();
        }).await.unwrap();
        for id in ["first", "second"] {
            control.live.remove_shell(id).await;
        }
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn thread_delete_replays_without_stopping_or_deleting_twice() {
        let root = tempfile::tempdir().unwrap();
        let owner =
            crate::app::DataOwner::start(None, Some(root.path().join("data")), root.path().into())
                .await
                .unwrap();
        let control = Control::new(&owner, root.path().into());
        let cwd = root.path().to_string_lossy().into_owned();
        control.store.database.call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('target',?1,'','fixture','fixture','now')",[cwd])?;
            db.execute_batch("CREATE TEMP TABLE delete_witness(kind TEXT);
                CREATE TEMP TRIGGER stop_witness AFTER UPDATE OF state ON main.shell_execution WHEN NEW.state='stopping' AND OLD.state<>'stopping' BEGIN INSERT INTO delete_witness VALUES('stop'); END;
                CREATE TEMP TRIGGER delete_witness AFTER UPDATE OF deleted_at ON main.thread WHEN OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL BEGIN INSERT INTO delete_witness VALUES('delete'); END;
                CREATE TEMP TRIGGER receipt_witness BEFORE UPDATE OF state ON main.operation_receipt WHEN NEW.operation_id LIKE '%:delete' AND NEW.state='completed' BEGIN SELECT CASE WHEN (SELECT deleted_at FROM main.thread WHERE id='target') IS NULL OR EXISTS(SELECT 1 FROM main.schedule WHERE thread_id='target') THEN RAISE(ABORT,'delete receipt must match deletion') END; END;
                CREATE TEMP TRIGGER deletion_after_receipt AFTER UPDATE OF deleted_at ON main.thread WHEN NEW.id='target' AND NEW.deleted_at IS NOT NULL BEGIN SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM main.operation_receipt WHERE operation_id LIKE '%:delete' AND state='pending') THEN RAISE(ABORT,'deletion needs pending receipt') END; END;")?;
            Ok(())
        }).await.unwrap();
        control.start().await.unwrap();
        let (sender, requests) = tokio::sync::mpsc::channel(4);
        let server = crate::ipc::LocalServer::bind(&owner, sender).await.unwrap();
        let dispatcher = tokio::spawn(control.clone().serve(requests));
        let request = |id, command| Request::new(format!("{}:{id}", owner.instance_id), command);
        let add = control
            .request(request(
                "add",
                Command::ScheduleAdd(crate::protocol::ScheduleAddArgs {
                    thread_id: Some("target".into()),
                    cron: None,
                    at: Some(Utc::now().to_rfc3339()),
                    in_seconds: None,
                    cwd: None,
                    description: None,
                    command: "sleep 60".into(),
                }),
            ))
            .await;
        assert!(add.ok, "{:?}", add.error);
        crate::test_watchdog::watchdog("scheduled shell registers before Thread deletion",async {
            loop {
                let changed=control.changed.notified();
                tokio::pin!(changed); changed.as_mut().enable();
                if control.store.database.call(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE schedule_id IS NOT NULL AND state='running')",[],|r|r.get::<_,bool>(0))?)).await.unwrap(){break;}
                changed.await;
            }
        }).await.unwrap();
        let command = Command::ThreadDelete(crate::protocol::ThreadDeleteArgs {
            thread_id: "target".into(),
            descendants: false,
            preview_marker: None,
        });
        let first = control.request(request("delete", command.clone())).await;
        let replay = control.request(request("delete", command)).await;
        assert!(first.ok, "{:?}", first.error);
        assert!(replay.ok, "{:?}", replay.error);
        assert_eq!(first.result, replay.result);
        control
            .store
            .database
            .call(|db| {
                let facts = db
                    .prepare("SELECT kind FROM delete_witness ORDER BY rowid")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert_eq!(facts, ["stop", "delete"]);
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM shell_execution WHERE schedule_id IS NOT NULL",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
        dispatcher.await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn removing_claimed_one_shot_accepts_proven_retirement_not_absence() {
        let root = tempfile::tempdir().unwrap();
        let owner =
            crate::app::DataOwner::start(None, Some(root.path().join("data")), root.path().into())
                .await
                .unwrap();
        let control = Control::new(&owner, root.path().into());
        let cwd = root.path().to_string_lossy().into_owned();
        owner.db.call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t',?1,'','p','m','now')",[&cwd])?;
            db.execute("INSERT INTO schedule(id,thread_id,once_at,enabled,cwd,command) VALUES('q','t',1,1,?1,'echo fixture')",[&cwd])?;
            db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('x','q','x','starting',?1,'{}','utf-8','utf-8','closed','2020-01-01T00:00:00Z')",[cwd])?;
            Ok(())
        }).await.unwrap();
        let request = || {
            Request::new(
                format!("{}:{}", owner.instance_id, uuid::Uuid::new_v4()),
                Command::ScheduleRemove(crate::protocol::ScheduleTarget {
                    schedule_id: "q".into(),
                }),
            )
        };
        let first = request();
        let running = control.clone();
        let call = first.clone();
        let deleting = tokio::spawn(async move { running.request(call).await });
        // Read the prepared active execution before allowing the pre-spawn stop
        // to settle. The DB writer has already committed the stop intent.
        crate::test_watchdog::watchdog("remove racing one-shot retirement", async {
            while !owner
                .db
                .call(|db| {
                    Ok(db.query_row(
                        "SELECT state='stopping' FROM shell_execution WHERE id='x'",
                        [],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap()
            {
                tokio::task::yield_now().await;
            }
            assert!(control.shell_start_intent("x", false).await.unwrap().0);
            let result = deleting.await.unwrap();
            assert!(result.ok, "{:?}", result.error);
            assert_eq!(result.result, control.request(first).await.result);
        })
        .await
        .unwrap();
        assert_eq!(
            control.request(request()).await.error.unwrap().code,
            crate::protocol::ErrorCode::TargetUnavailable
        );
        assert!(
            control
                .request(Request::new(
                    format!("{}:get", owner.instance_id),
                    Command::ShellGet(crate::protocol::ExecutionTarget {
                        execution_id: "x".into()
                    })
                ))
                .await
                .ok
        );
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn scheduled_shell_settlement_preserves_the_execution_and_retires_only_one_shots() {
        use crate::protocol::{
            ExecutionTarget, ScheduleAddArgs, ShellListArgs, ShellReadArgs, ThreadTarget,
        };

        let (_root, owner, control, accepted) =
            super::super::driver::preparation_tests::fixture("http://127.0.0.1:1/v1").await;
        let thread = accepted.thread_id;
        let request = |command| {
            Request::new(
                format!("{}:{}", owner.instance_id, uuid::Uuid::new_v4()),
                command,
            )
        };
        let hold = if cfg!(windows) {
            "echo schedule-witness; Start-Sleep -Seconds 600"
        } else {
            "echo schedule-witness; exec sleep 600"
        };
        for (command, expected, timeout, cron, missing_cwd) in [
            (hold, "stopped", false, false, false),
            (hold, "stopped", true, false, false),
            ("echo schedule-witness", "exited", false, false, false),
            (
                "echo schedule-witness; exit 7",
                "exited",
                false,
                false,
                false,
            ),
            ("echo schedule-witness", "failed", false, false, true),
            ("echo schedule-witness", "exited", false, true, false),
        ] {
            let add = control
                .request(request(Command::ScheduleAdd(ScheduleAddArgs {
                    thread_id: Some(thread.clone()),
                    cron: cron.then(|| "0 0 1 1 *".into()),
                    at: None,
                    in_seconds: (!cron).then_some(1),
                    cwd: missing_cwd.then(|| "missing-working-directory".into()),
                    description: None,
                    command: command.into(),
                })))
                .await;
            assert!(add.ok, "{:?}", add.error);
            let schedule = add.result.unwrap()["schedule_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let key = schedule.clone();
            // Backdate the saved deadline instead of waiting for the clock. The
            // normal dispatch path still performs claim, launch and registration.
            owner.db.call(move |db| {
                db.execute("UPDATE schedule SET next_at=1,once_at=CASE WHEN once_at IS NOT NULL THEN 1 END WHERE id=?1",[key])?;
                Ok(())
            }).await.unwrap();
            let list = || {
                Command::ShellList(
                    serde_json::from_value::<ShellListArgs>(json!({"thread_id":thread})).unwrap(),
                )
            };
            let before = control.request(request(list())).await.result.unwrap();
            dispatch(&control).await.unwrap();
            let after = control.request(request(list())).await.result.unwrap();
            let mut added = after["items"].as_array().unwrap().iter().filter(|item| {
                !before["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|prior| prior["execution_id"] == item["execution_id"])
            });
            let execution = added.next().expect("dispatch adds an execution")["execution_id"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(added.next().is_none(), "dispatch adds only one execution");
            let get = || {
                Command::ShellGet(ExecutionTarget {
                    execution_id: execution.clone(),
                })
            };
            if command == hold {
                let active = control.request(request(list())).await.result.unwrap();
                let active = active["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["execution_id"] == execution)
                    .unwrap();
                assert_eq!(active["state"], "running");
                assert_eq!(active["schedule_id"], schedule);
                let id = execution.clone();
                let deadline = owner
                    .db
                    .call(move |db| {
                        let started: i64 = db.query_row(
                            "SELECT unixepoch(started_at) FROM shell_execution WHERE id=?1",
                            [id],
                            |r| r.get(0),
                        )?;
                        Ok(DateTime::from_timestamp(started + 3600, 0).unwrap())
                    })
                    .await
                    .unwrap();
                assert_eq!(next_deadline(&control).await.unwrap(), Some(deadline));
                if timeout {
                    let id = execution.clone();
                    owner.db.call(move |db| {
                        db.execute("UPDATE shell_execution SET started_at='2020-01-01T00:00:00Z' WHERE id=?1",[id])?;
                        Ok(())
                    }).await.unwrap();
                    dispatch(&control).await.unwrap();
                } else {
                    let stop = control
                        .request(request(Command::ShellStop(ExecutionTarget {
                            execution_id: execution.clone(),
                        })))
                        .await;
                    assert_eq!(stop.result.unwrap()["accepted"], true);
                }
            }
            let saved = crate::test_watchdog::watchdog("scheduled execution settles", async {
                loop {
                    let changed = control.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    let result = control.request(request(get())).await;
                    assert!(result.ok, "{:?}", result.error);
                    let body = result.result.unwrap();
                    if matches!(
                        body["state"].as_str(),
                        Some("exited" | "failed" | "stopped")
                    ) {
                        break body;
                    }
                    changed.await;
                }
            })
            .await
            .unwrap();
            assert_eq!(saved["execution_id"], execution);
            assert_eq!(saved["thread_id"], thread);
            assert_eq!(saved["state"], expected);
            if expected == "exited" {
                assert_eq!(
                    saved["exit_code"],
                    if command.ends_with("exit 7") { 7 } else { 0 }
                );
                let read = control
                    .request(request(Command::ShellRead(ShellReadArgs {
                        execution_id: execution.clone(),
                        cursor: None,
                        encoding: None,
                        limit_bytes: 4096,
                    })))
                    .await;
                assert!(
                    read.result.unwrap()["text"]
                        .as_str()
                        .unwrap()
                        .contains("schedule-witness")
                );
            }
            if timeout {
                assert_eq!(saved["reason"], "schedule_timeout");
            }
            if missing_cwd {
                assert_eq!(saved["reason"], "spawn_failed");
            }
            let history = control.request(request(list())).await.result.unwrap();
            assert!(
                history["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["execution_id"] == execution)
            );
            let schedules = control
                .request(request(Command::ScheduleList(ThreadTarget {
                    thread_id: Some(thread.clone()),
                })))
                .await
                .result
                .unwrap();
            let retained = schedules["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["schedule_id"] == schedule);
            if cron {
                let item = retained.unwrap();
                assert_eq!(item["latest"]["execution_id"], execution);
                assert!(item["next_at"].is_string());
            } else {
                assert!(retained.is_none());
            }
        }
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
