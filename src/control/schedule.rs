//! Advance a scheduled occurrence and reserve its ordinary shell in one transaction.
use super::{Control, ShellToolArgs, driver::shell_output};
use crate::protocol::ApiError;
use crate::storage::StorageError;
use chrono::{DateTime, Local, Utc};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{path::PathBuf, sync::Arc};

struct DueShell {
    id: String,
    thread: String,
    cwd: PathBuf,
    command: String,
}

fn next_cron(expression: &str, now: DateTime<Utc>) -> Option<i64> {
    if expression.split_whitespace().count() != 5 {
        return None;
    }
    expression
        .parse::<croner::Cron>()
        .ok()?
        .find_next_occurrence(&now.with_timezone(&Local), false)
        .ok()
        .map(|next| next.timestamp())
}

fn advance_due(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    shell_kind: &str,
) -> Result<Option<DueShell>, StorageError> {
    let due = tx.query_row(
        "SELECT id,thread_id,cwd,command,cron,next_at FROM schedule WHERE enabled=1 AND next_at IS NOT NULL AND next_at<=?1 ORDER BY next_at,id LIMIT 1", [now.timestamp()],
        |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?)),
    ).optional()?;
    let Some((schedule, thread, cwd, command, cron, scheduled_at)) = due else {
        return Ok(None);
    };
    let next = cron.as_deref().and_then(|cron| next_cron(cron, now));
    let invalid = cron.is_some() && next.is_none();
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

pub(super) async fn dispatch(control: &Arc<Control>) -> Result<Option<DateTime<Utc>>, ApiError> {
    let kind = serde_json::to_value(control.store.config.shell.kind).unwrap();
    let (shell, next, expired) = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let now = Utc::now();
            let expired = tx.query_row("UPDATE shell_execution SET state='stopping', end_reason='schedule_timeout' WHERE id=(SELECT id FROM shell_execution WHERE schedule_id IS NOT NULL AND state IN ('starting','running') AND unixepoch(started_at)<=?1 ORDER BY started_at,id LIMIT 1) RETURNING id",[now.timestamp()-3600],|r|r.get::<_,String>(0)).optional()?;
            let shell = advance_due(&tx, Utc::now(), kind.as_str().unwrap_or_default())?;
            let next = tx.query_row(
                "SELECT min(deadline) FROM (SELECT min(next_at) AS deadline FROM schedule WHERE enabled=1 AND next_at IS NOT NULL UNION ALL SELECT min(unixepoch(started_at)+3600) FROM shell_execution WHERE schedule_id IS NOT NULL AND state IN ('starting','running'))",
                [],
                |r| r.get::<_, Option<i64>>(0),
            )?;
            tx.commit()?;
            Ok((
                shell,
                next.and_then(|unix| DateTime::from_timestamp(unix, 0)),
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
        let env = shell_output::shell_environment(control, &shell.thread);
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
                        super::state::fail_shell_spawn(db, &shell.id, &super::state::now())?;
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
        (Some(cron), None, None) => (None, next_cron(cron, now)),
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
                        next_cron(&cron, now)
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
    let ids=tx.prepare(&format!("UPDATE shell_execution SET state='stopping' WHERE schedule_id IN (SELECT id FROM schedule WHERE {column}=?1) AND state IN ('starting','running','stopping') RETURNING id"))?.query_map([key],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

pub(super) async fn delete_thread(
    control: &Arc<Control>,
    args: crate::protocol::ThreadDeleteArgs,
) -> Result<serde_json::Value, ApiError> {
    let request = args.clone();
    let executions = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let mut executions = Vec::new();
            for thread in super::db::delete_targets(&tx, &request).map_err(StorageError::from)? {
                executions.extend(disable_and_stop(&tx, &thread, true)?);
            }
            tx.commit()?;
            Ok(executions)
        })
        .await
        .map_err(super::storage_error)?;
    drain(control, executions).await?;
    let effects = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let result = super::db::delete_thread(&tx, &args, &super::state::now())
                .map_err(StorageError::from)?;
            tx.commit()?;
            Ok(result)
        })
        .await
        .map_err(super::storage_error)?;
    let signals = control.store.database.signals();
    signals.mark_all(&crate::maintenance::Work::DELETION);
    signals.mark_all(&[
        crate::maintenance::Work::Managed,
        crate::maintenance::Work::Drafts,
        crate::maintenance::Work::Schedule,
    ]);
    let value = effects.result.clone();
    control.apply_effects(Some(effects)).await;
    Ok(value)
}

pub(super) async fn remove(control: &Control, id: String) -> Result<serde_json::Value, ApiError> {
    let target = id.clone();
    let (thread, executions) = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            let thread = tx
                .query_row(
                    "SELECT thread_id FROM schedule WHERE id=?1",
                    [&target],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    ApiError::new(
                        crate::protocol::ErrorCode::TargetUnavailable,
                        "Schedule not found",
                    )
                })?;
            let executions = disable_and_stop(&tx, &target, false)?;
            tx.commit()?;
            Ok((thread, executions))
        })
        .await
        .map_err(super::storage_error)?;
    drain(control, executions).await?;
    let target = id.clone();
    control.store.database.call(move |db| {
        if db.execute("DELETE FROM schedule WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM shell_execution WHERE schedule_id=?1 AND state IN ('starting','running','stopping'))",[target])?!=1 {
            return Err(ApiError::new(crate::protocol::ErrorCode::OperationConflict,"Schedule changed before removal").into());
        }
        Ok(())
    }).await.map_err(super::storage_error)?;
    control
        .store
        .database
        .signals()
        .mark(crate::maintenance::Work::Schedule);
    control.changed(&thread);
    Ok(serde_json::json!({"schedule_id":id,"removed":true}))
}

async fn drain(control: &Control, executions: Vec<String>) -> Result<(), ApiError> {
    for id in executions {
        if let Some(handle) = control.live.clone_shell_handle(&id).await {
            handle.stop();
        }
        loop {
            let changed = control.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let target = id.clone();
            if !control.store.database.call(move |db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE id=?1 AND state IN ('starting','running','stopping'))",[target],|r|r.get::<_,bool>(0))?)).await.map_err(super::storage_error)? { break; }
            changed.await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Command, Request};
    use serde_json::json;

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
                CREATE TEMP TRIGGER delete_witness AFTER UPDATE OF deleted_at ON main.thread WHEN OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL BEGIN INSERT INTO delete_witness VALUES('delete'); END;")?;
            Ok(())
        }).await.unwrap();
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
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn scheduled_shell_runs_once_and_list_reports_saved_result() {
        let (root, owner, control, accepted) =
            super::super::driver::preparation_tests::fixture("http://127.0.0.1:1/v1").await;
        let thread = accepted.thread_id;
        let request = |command| {
            Request::new(
                format!("{}:{}", owner.instance_id, uuid::Uuid::new_v4()),
                command,
            )
        };
        control.start().await.unwrap();
        let add = control
            .request(request(Command::ScheduleAdd(
                crate::protocol::ScheduleAddArgs {
                    thread_id: Some(thread.clone()),
                    cron: None,
                    at: Some(Utc::now().to_rfc3339()),
                    in_seconds: None,
                    cwd: None,
                    description: None,
                    command: "echo schedule-witness".into(),
                },
            )))
            .await;
        assert!(add.ok, "{:?}", add.error);
        crate::test_watchdog::watchdog("schedule add through saved shell result", async {
            loop {
                let changed = control.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let list = control
                    .request(request(Command::ScheduleList(
                        crate::protocol::ThreadTarget {
                            thread_id: Some(thread.clone()),
                        },
                    )))
                    .await;
                assert!(list.ok, "{:?}", list.error);
                let body = list.result.unwrap();
                if body["items"][0]["latest"]["state"] == "exited" {
                    assert_eq!(body["items"][0]["latest"]["exit_code"], 0);
                    assert!(
                        body["items"][0]["latest"]["output_tail"]
                            .as_str()
                            .unwrap()
                            .contains("schedule-witness")
                    );
                    assert_eq!(body["items"][0]["next_at"], json!(null));
                    break;
                }
                changed.await;
            }
        })
        .await
        .unwrap();
        control.store.database.call(|db| {
            assert_eq!(db.query_row("SELECT count(*) FROM shell_execution WHERE schedule_id IS NOT NULL AND owner_run_id IS NULL AND turn_id IS NULL",[],|r|r.get::<_,i64>(0))?,1);
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        drop(root);
    }
}
