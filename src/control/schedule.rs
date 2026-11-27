//! Advance a scheduled occurrence and reserve its ordinary shell in one transaction.
use super::{
    Control, ShellToolArgs,
    driver::shell_output,
    runtime::{ShellStopIntent, record_shell_stop},
};
use crate::protocol::{ApiError, ScheduleNotify};
use crate::storage::StorageError;
use chrono::{DateTime, Local, TimeZone, Utc};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{path::PathBuf, sync::Arc};

struct DueShell {
    id: String,
    thread: String,
    cwd: PathBuf,
    command: String,
    service: bool,
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

#[derive(Default)]
struct Advanced {
    shell: Option<DueShell>,
    notice: Option<super::MutationResult>,
    /// The reservation made an execution whose raw output was already
    /// collected no longer its definition's latest; that definition's
    /// raw-marker settlement is recorded in the same transaction and the
    /// caller marks the marker owner after commit (#450).
    superseded: bool,
}

fn advance_due(
    store: &super::Store,
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    shell_kind: &str,
) -> Result<Advanced, StorageError> {
    let due = tx.query_row(
        "SELECT id,thread_id,cwd,command,cron,next_at,kind,EXISTS(SELECT 1 FROM json_each(notify_json) WHERE value=?1) FROM schedule WHERE enabled=1 AND next_at IS NOT NULL ORDER BY next_at,id LIMIT 1", [ScheduleNotify::Skipped.as_str()],
        |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,bool>(7)?)),
    ).optional()?;
    let Some((schedule, thread, cwd, command, cron, scheduled_at, kind, notify_skipped)) = due
    else {
        return Ok(Advanced::default());
    };
    let Occurrence::Due { next, invalid } = occurrence(scheduled_at, cron.as_deref(), now, &Local)
    else {
        return Ok(Advanced::default());
    };
    let active: Option<String> = tx.query_row("SELECT id FROM shell_execution WHERE schedule_id=?1 AND state IN ('starting','running','stopping') ORDER BY rowid DESC LIMIT 1",[&schedule],|r| r.get(0)).optional()?;
    tx.execute("UPDATE schedule SET next_at=?2,last_skipped_at=CASE WHEN ?3 THEN ?4 ELSE last_skipped_at END WHERE id=?1",params![schedule,next,active.is_some(),scheduled_at])?;
    let notice = match &active {
        Some(execution) if notify_skipped => {
            skipped_notice(store, tx, &schedule, &thread, scheduled_at, execution)?
        }
        _ => None,
    };
    if active.is_some() && !invalid {
        return Ok(Advanced {
            shell: None,
            notice,
            superseded: false,
        });
    }
    let superseded = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||(SELECT id FROM shell_execution WHERE schedule_id=?1 ORDER BY rowid DESC LIMIT 1) AND CASE WHEN json_valid(value) THEN json_extract(value,'$.state') END='deleted')",
        [&schedule],
        |r| r.get(0),
    )?;
    if superseded {
        crate::gc::request_definition_settlement_tx(tx, &schedule)?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let shell = serde_json::json!({"command": command,"kind":shell_kind});
    let at = now.to_rfc3339();
    tx.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,end_reason,output_complete) VALUES(?1,?2,?1,?3,?4,?5,'utf-8','utf-8','closed',?6,?7,?8,?9)",params![id,schedule,if invalid {"failed"} else {"starting"},cwd,shell.to_string(),at,invalid.then_some(&at),invalid.then_some("schedule_recurrence_calculation_failed"),invalid.then_some(false)])?;
    Ok(Advanced {
        shell: (!invalid).then(|| DueShell {
            id,
            thread,
            cwd: cwd.into(),
            command,
            service: kind == "service",
        }),
        notice,
        superseded,
    })
}

/// One notice per skipped occurrence, keyed by schedule and scheduled time, in
/// the transaction that advances `next_at`. An occurrence already noticed (a
/// re-enabled schedule can meet the same time again) sends nothing.
fn skipped_notice(
    store: &super::Store,
    tx: &Transaction<'_>,
    schedule: &str,
    thread: &str,
    scheduled_at: i64,
    execution: &str,
) -> Result<Option<super::MutationResult>, StorageError> {
    let operation = format!("schedule-skipped:{schedule}:{scheduled_at}");
    let sent: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM receipt WHERE client_operation_id=?1)",
        [&operation],
        |r| r.get(0),
    )?;
    if sent {
        return Ok(None);
    }
    let at = DateTime::from_timestamp(scheduled_at, 0)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(5, scheduled_at))?
        .to_rfc3339();
    let args = crate::protocol::MessageSendArgs {
        max_model_requests: None,
        human: false,
        resume_recovery: false,
        sender_label: Some(format!("schedule:{schedule}")),
        thread_id: Some(thread.to_owned()),
        deliver: crate::protocol::Deliver::Next,
        text: format!(
            "Schedule {schedule} skipped {at}: execution {execution} is still running. Inspect: shellboy shell read --execution {execution}. Stop: shellboy shell stop --execution {execution}. Stop these notices: shellboy schedule set --schedule {schedule} --notify none"
        ),
    };
    Ok(Some(store.send_owner_notice(
        tx,
        &args,
        &operation,
        &super::state::now(),
    )?))
}

fn read_deadline(db: &rusqlite::Connection) -> rusqlite::Result<Option<DateTime<Utc>>> {
    db.query_row(
        "SELECT min(deadline) FROM (SELECT min(next_at) AS deadline FROM schedule WHERE enabled=1 AND next_at IS NOT NULL UNION ALL SELECT min(unixepoch(s.started_at)+3600) FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE q.kind='time' AND s.state IN ('starting','running'))",
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
    let owner = control.clone();
    let wakes = control.waits.keyed.clone();
    let (advanced, next, expired) = control
        .store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            if owner.admission.is_closed() {
                return Ok((Advanced::default(), None, None));
            }
            let now = Utc::now();
            let expired: Option<String> = tx.query_row("SELECT s.id FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE q.kind='time' AND s.state IN ('starting','running') AND unixepoch(s.started_at)<=?1 ORDER BY s.started_at,s.id LIMIT 1",[now.timestamp()-3600],|r|r.get(0)).optional()?;
            if let Some(id) = &expired {
                record_shell_stop(&tx,id,ShellStopIntent::ScheduleTimeout)?;
            }
            let mut advanced = advance_due(&owner.store, &tx, now, kind.as_str().unwrap_or_default())?;
            let next = read_deadline(&tx)?;
            tx.commit()?;
            if let Some(notice) = &mut advanced.notice {
                wakes.publish(std::mem::take(&mut notice.wait_keys));
            }
            Ok((
                advanced,
                next,
                expired,
            ))
        })
        .await
        .map_err(super::storage_error)?;
    if advanced.superseded {
        control
            .store
            .database
            .signals()
            .mark(crate::maintenance::Work::RawMarkers);
    }
    if let Some(id) = expired
        && let Some(handle) = control.live.clone_shell_handle(&id).await
    {
        handle.stop();
    }
    control.apply_effects(advanced.notice).await;
    if let Some(shell) = advanced.shell {
        spawn_reserved(control, shell).await?;
    }
    Ok(next)
}

async fn spawn_reserved(control: &Arc<Control>, shell: DueShell) -> Result<(), ApiError> {
    if control.shell_start_intent(&shell.id, false, None).await?.0 {
        control.changed(&shell.thread);
        return Ok(());
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
    let mut env = shell_output::cli_environment(control, &shell.thread);
    let endpoint = if shell.service {
        super::service_endpoint::Publication::create(control, &shell.id)
            .await
            .map(Some)
    } else {
        Ok(None)
    };
    let (spawned, endpoint) = match endpoint {
        Ok(endpoint) => {
            if let Some(file) = &endpoint {
                env.insert(super::service_endpoint::ENV.into(), file.env_value());
            }
            let spawned = control
                .shell
                .spawn_thread_owned(shell.id.clone(), shell.cwd, args, env)
                .await;
            (spawned, endpoint)
        }
        Err(error) => (Err(error), None),
    };
    match spawned {
        Ok(spawned) => {
            shell_output::install_shell(
                control,
                shell_output::ShellOwner::Schedule {
                    thread: shell.thread,
                    endpoint: endpoint.map(Arc::new),
                },
                None,
                &shell.id,
                spawned,
            )
            .await?;
        }
        Err(error) => {
            let settlement_detail = match endpoint {
                Some(publication) => publication.retire(control).await,
                None => None,
            };
            let shutdown = error
                .details
                .as_ref()
                .is_some_and(|details| details["reason"] == "shutdown_before_spawn");
            let store = control.store.clone();
            let wakes = control.waits.keyed.clone();
            let saved_error = error.clone();
            let effects = control
                .store
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let service = service_settlement(&tx, &shell.id)?;
                    let mut effects = None;
                    let at = super::state::now();
                    let changed = if shutdown {
                        record_shell_stop(&tx, &shell.id, ShellStopIntent::OwnerShutdown)?;
                        super::state::cancel_shell_spawn(&tx, &shell.id, &at)? != 0
                    } else {
                        super::state::fail_shell_spawn(&tx, &shell.id, &at, &saved_error)? != 0
                    };
                    if changed {
                        if let Some(detail) = settlement_detail {
                            tx.execute("UPDATE shell_execution SET shell_json=json_set(shell_json,'$.completion.settlement_detail',json(?2)) WHERE id=?1", params![shell.id, serde_json::json!(detail).to_string()])?;
                        }
                        if service.is_none() {
                            effects = refused_oneshot_notice(&store, &tx, &shell.id, &saved_error)?;
                        }
                        retire_oneshot(&tx, &shell.id)?;
                        if let Some(service) = service.filter(|_| !shutdown) {
                            effects = service.finish(&store, &tx, &shell.id, Some(&saved_error))?;
                        }
                    }
                    tx.commit()?;
                    if changed {
                        wakes.publish(
                            super::wait::wakes::Key::shell_terminal(shell.id)
                                .into_iter()
                                .chain(
                                    effects
                                        .iter_mut()
                                        .flat_map(|effects| std::mem::take(&mut effects.wait_keys)),
                                ),
                        );
                    }
                    Ok(effects)
                })
                .await
                .map_err(super::storage_error)?;
            control.apply_effects(effects).await;
            control.changed(&shell.thread);
        }
    }
    Ok(())
}

/// What a descriptor_budget refusal tells the Thread: the live numbers and the
/// next action. Other spawn failures add nothing.
fn refusal_text(error: &ApiError) -> Option<String> {
    let details = error.details.as_ref()?;
    if details["reason"] != crate::shell::DESCRIPTOR_BUDGET_REASON {
        return None;
    }
    let count = |key: &str| {
        details[key]
            .as_u64()
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
    };
    Some(format!(
        "descriptor_budget: {} of {} file descriptors were open. Stop unneeded shells first (shellboy shell list --state running, then shellboy shell stop --execution EXECUTION_ID)",
        count("open"),
        count("limit")
    ))
}

/// A one-shot schedule is retired when its start fails; a descriptor_budget
/// refusal says so and names the recovery, in the same transaction.
fn refused_oneshot_notice(
    store: &super::Store,
    tx: &Transaction<'_>,
    execution: &str,
    error: &ApiError,
) -> Result<Option<super::MutationResult>, StorageError> {
    let Some(reason) = refusal_text(error) else {
        return Ok(None);
    };
    let owner: Option<(String, String)> = tx
        .query_row(
            "SELECT q.id,q.thread_id FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE s.id=?1 AND q.kind='time' AND q.once_at IS NOT NULL AND q.next_at IS NULL",
            [execution],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((schedule, thread)) = owner else {
        return Ok(None);
    };
    let args = crate::protocol::MessageSendArgs {
        max_model_requests: None,
        human: false,
        resume_recovery: false,
        sender_label: Some(format!("schedule:{schedule}")),
        thread_id: Some(thread),
        deliver: crate::protocol::Deliver::Next,
        text: format!(
            "One-shot schedule {schedule} did not run and was removed: execution {execution} was refused. {reason}; then recreate the one-shot schedule."
        ),
    };
    Ok(Some(store.send_owner_notice(
        tx,
        &args,
        &format!("schedule-refused:{execution}"),
        &super::state::now(),
    )?))
}

// Called only for an execution just settled in this transaction. A preexisting
// spent one-shot is not a retirement candidate on startup or migration.
pub(super) fn retire_oneshot(tx: &Transaction<'_>, execution: &str) -> rusqlite::Result<()> {
    let owner: Option<(String, String)> = tx.query_row(
        "SELECT q.id,q.thread_id FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE s.id=?1 AND s.state IN ('exited','failed','stopped') AND q.kind='time' AND q.once_at IS NOT NULL AND q.next_at IS NULL",
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
    add_definition(
        tx,
        thread,
        DefinitionInput {
            command: &a.command,
            cwd: a.cwd.as_deref(),
            description: a.description.as_deref(),
            timing: Some(a),
            name: None,
        },
    )
}

pub(super) fn add_service(
    tx: &Transaction<'_>,
    thread: &str,
    a: &crate::protocol::ServiceAddArgs,
) -> Result<serde_json::Value, ApiError> {
    add_definition(
        tx,
        thread,
        DefinitionInput {
            command: &a.command,
            cwd: a.cwd.as_deref(),
            description: a.description.as_deref(),
            timing: None,
            name: a.name.as_deref(),
        },
    )
}

struct DefinitionInput<'a> {
    command: &'a str,
    cwd: Option<&'a str>,
    description: Option<&'a str>,
    timing: Option<&'a crate::protocol::ScheduleAddArgs>,
    name: Option<&'a str>,
}

fn add_definition(
    tx: &Transaction<'_>,
    thread: &str,
    input: DefinitionInput<'_>,
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
    if input.command.is_empty() || input.command.len() > 32 * 1024 || input.command.contains('\0') {
        return Err(invalid(
            "Shell command must be 1 to 32768 bytes without NUL",
        ));
    }
    if input
        .description
        .as_ref()
        .is_some_and(|s| s.chars().count() > crate::tool_description::MAX_CHARS)
    {
        return Err(invalid("Shell description exceeds the description limit"));
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM schedule WHERE thread_id=?1",
            [thread],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if count >= 64 {
        return Err(invalid(
            "A Thread may have at most 64 schedules and services combined",
        ));
    }
    let now = Utc::now();
    let notify = serde_json::to_string(&ScheduleNotify::saved(
        input.timing.map_or(&[][..], |a| &a.notify),
    )?)
    .expect("notice kinds encode");
    let (kind, cron, once, next) = if let Some(a) = input.timing {
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
        (
            "time",
            a.cron.as_deref(),
            once,
            next.ok_or_else(|| invalid("Cannot calculate the schedule time"))?,
        )
    } else {
        ("service", None, None, now.timestamp())
    };
    let cwd = PathBuf::from(cwd).join(input.cwd.unwrap_or(""));
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT INTO schedule(id,thread_id,kind,cron,once_at,next_at,enabled,cwd,command,description,notify_json,name) VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8,?9,?10,?11)",params![id,thread,kind,cron,once,next,cwd.to_string_lossy(),input.command,input.description,notify,input.name]).map_err(service_name_write_error)?;
    let key = if kind == "service" {
        "service_id"
    } else {
        "schedule_id"
    };
    Ok(
        serde_json::json!({(key):id,"thread_id":thread,"next_at":DateTime::from_timestamp(next,0).map(|at|at.to_rfc3339())}),
    )
}

pub(super) fn list(
    tx: &Transaction<'_>,
    thread: &str,
    services: bool,
) -> Result<serde_json::Value, ApiError> {
    let items = definition_rows(tx, thread, services, None)?;
    Ok(serde_json::json!({"thread_id":thread,"items":items}))
}

fn definition_rows(
    tx: &Transaction<'_>,
    thread: &str,
    services: bool,
    service_id: Option<&str>,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let sql = |e| super::storage_error(StorageError::from(e));
    let mut query=tx.prepare("SELECT q.id,q.enabled,q.next_at,q.cron,q.once_at,q.cwd,q.command,q.description,q.last_skipped_at,s.id,s.state,s.exit_code,s.end_reason,s.started_at,s.ended_at,q.notify_json,q.name FROM schedule q JOIN thread t ON t.id=q.thread_id AND t.deleted_at IS NULL LEFT JOIN shell_execution s ON s.id=(SELECT id FROM shell_execution WHERE schedule_id=q.id ORDER BY rowid DESC LIMIT 1) WHERE q.thread_id=?1 AND q.kind=?2 AND (?3 IS NULL OR q.id=?3) ORDER BY q.id LIMIT 64").map_err(sql)?;
    let items=query.query_map(params![thread, if services { "service" } else { "time" }, service_id],|r| {
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
        if services {
            return Ok(serde_json::json!({"service_id":r.get::<_,String>(0)?,"name":r.get::<_,Option<String>>(16)?,"enabled":r.get::<_,bool>(1)?,"cwd":r.get::<_,String>(5)?,"command":r.get::<_,String>(6)?,"description":r.get::<_,Option<String>>(7)?,"latest":latest}));
        }
        let notify: Vec<ScheduleNotify> = serde_json::from_str(&r.get::<_, String>(15)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(15, rusqlite::types::Type::Text, Box::new(error)))?;
        Ok(serde_json::json!({"schedule_id":r.get::<_,String>(0)?,"enabled":r.get::<_,bool>(1)?,"next_at":timestamp(2)?,"cron":r.get::<_,Option<String>>(3)?,"at":timestamp(4)?,"cwd":r.get::<_,String>(5)?,"command":r.get::<_,String>(6)?,"description":r.get::<_,Option<String>>(7)?,"notify":notify,"last_skipped_at":timestamp(8)?,"latest":latest}))
    }).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    Ok(items)
}

fn service_name_write_error(error: rusqlite::Error) -> ApiError {
    if matches!(&error, rusqlite::Error::SqliteFailure(e, _) if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
    {
        return ApiError::new(
            crate::protocol::ErrorCode::InvalidArgument,
            "A service already has this name in the Thread",
        )
        .details(serde_json::json!({"reason":"service_name_conflict"}));
    }
    super::storage_error(StorageError::from(error))
}

pub(super) fn service_not_found() -> ApiError {
    ApiError::new(
        crate::protocol::ErrorCode::TargetUnavailable,
        "Service not found",
    )
    .details(serde_json::json!({"reason":"service_not_found"}))
}

pub(super) fn get_service(tx: &Transaction<'_>, id: &str) -> Result<serde_json::Value, ApiError> {
    let thread: String = tx
        .query_row(
            "SELECT thread_id FROM schedule WHERE id=?1 AND kind='service'",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(StorageError::from)
        .map_err(super::storage_error)?
        .ok_or_else(service_not_found)?;
    let mut saved = definition_rows(tx, &thread, true, Some(id))?
        .pop()
        .ok_or_else(service_not_found)?;
    saved["thread_id"] = thread.into();
    Ok(saved)
}

pub(super) fn set_service_name(
    tx: &Transaction<'_>,
    args: &crate::protocol::ServiceSetArgs,
) -> Result<super::MutationResult, ApiError> {
    let name = (!args.name.is_empty()).then_some(args.name.as_str());
    let thread: String = tx
        .query_row(
            "UPDATE schedule SET name=?2 WHERE id=?1 AND kind='service' RETURNING thread_id",
            params![args.service_id, name],
            |r| r.get(0),
        )
        .optional()
        .map_err(service_name_write_error)?
        .ok_or_else(service_not_found)?;
    let mut result = super::MutationResult::value(
        serde_json::json!({"service_id":args.service_id,"name":name}),
    )?;
    result.changed_threads.push(thread);
    Ok(result)
}

pub(super) fn start_service(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<super::MutationResult, ApiError> {
    let thread: Option<String> = tx.query_row(
        "UPDATE schedule SET next_at=CASE WHEN enabled=0 THEN ?2 ELSE next_at END,enabled=1 WHERE id=?1 AND kind='service' AND NOT EXISTS(SELECT 1 FROM shell_execution WHERE schedule_id=?1 AND state='stopping') RETURNING thread_id",
        params![id,Utc::now().timestamp()], |r| r.get(0),
    ).optional().map_err(StorageError::from).map_err(super::storage_error)?;
    let Some(thread) = thread else {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schedule WHERE id=?1 AND kind='service')",
                [id],
                |row| row.get(0),
            )
            .map_err(StorageError::from)
            .map_err(super::storage_error)?;
        use crate::protocol::ErrorCode;
        return Err(if exists {
            ApiError::new(
                ErrorCode::InvalidState,
                "Service is stopping; retry after it stops",
            )
        } else {
            ApiError::new(ErrorCode::TargetUnavailable, "Service not found")
        });
    };
    let mut result = super::MutationResult::value(serde_json::json!({"service_id":id}))?;
    result.changed_threads.push(thread);
    Ok(result)
}

pub(super) enum Change<'a> {
    Definition {
        description: Option<&'a str>,
        notify: Option<&'a [ScheduleNotify]>,
    },
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
            "SELECT thread_id,cron,once_at,enabled FROM schedule WHERE id=?1 AND kind='time'",
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
        Change::Definition {
            description,
            notify,
        } => {
            if let Some(text) = description {
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
            if let Some(notify) = notify {
                let saved = serde_json::to_string(&ScheduleNotify::saved(notify)?)
                    .expect("notice kinds encode");
                tx.execute(
                    "UPDATE schedule SET notify_json=?2 WHERE id=?1",
                    params![id, saved],
                )
                .map_err(sql)?;
            }
        }
        Change::Enabled(next_enabled) if next_enabled != enabled => {
            let now = Utc::now();
            let next = if next_enabled {
                match cron {
                    Some(cron) => Some(
                        next_cron(&cron, now, &Local)
                            .ok_or_else(|| invalid("Cannot calculate the schedule time"))?,
                    ),
                    None => Some(once.filter(|value| *value > now.timestamp()).ok_or_else(
                        || {
                            ApiError::new(
                                crate::protocol::ErrorCode::InvalidState,
                                "The one-shot schedule has expired; add a new schedule",
                            )
                        },
                    )?),
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

pub(super) fn prepare_owner_shutdown(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let ids = tx.prepare("SELECT s.id FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE q.kind='service' AND s.state IN ('starting','running')")?
        .query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        record_shell_stop(tx, id, ShellStopIntent::OwnerShutdown)?;
    }
    Ok(ids)
}

pub(super) fn disable_services(
    tx: &Transaction<'_>,
    key: &str,
    by_thread: bool,
) -> rusqlite::Result<()> {
    let column = if by_thread { "thread_id" } else { "id" };
    tx.execute(
        &format!("UPDATE schedule SET enabled=0,next_at=NULL WHERE kind='service' AND {column}=?1"),
        [key],
    )?;
    Ok(())
}

pub(super) struct ServiceSettlement {
    id: String,
    thread: String,
    prior_state: String,
    prior_reason: Option<String>,
}

pub(super) fn service_settlement(
    tx: &Transaction<'_>,
    execution: &str,
) -> rusqlite::Result<Option<ServiceSettlement>> {
    tx.query_row("SELECT q.id,q.thread_id,s.state,s.end_reason FROM shell_execution s JOIN schedule q ON q.id=s.schedule_id WHERE s.id=?1 AND q.kind='service'", [execution], |r| Ok(ServiceSettlement {id:r.get(0)?,thread:r.get(1)?,prior_state:r.get(2)?,prior_reason:r.get(3)?})).optional()
}

impl ServiceSettlement {
    pub(super) fn finish(
        self,
        store: &super::Store,
        tx: &Transaction<'_>,
        execution: &str,
        refusal: Option<&ApiError>,
    ) -> Result<Option<super::MutationResult>, StorageError> {
        if self.prior_reason.as_deref() == Some("owner_shutdown") {
            return Ok(None);
        }
        disable_services(tx, &self.id, false)?;
        if !matches!(self.prior_state.as_str(), "starting" | "running") {
            return Ok(None);
        }
        let (state, code, reason, signal): (String,Option<i32>,Option<String>,Option<i32>) = tx.query_row("SELECT state,exit_code,end_reason,json_extract(shell_json,'$.completion.exit_signal') FROM shell_execution WHERE id=?1",[execution],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let id = self.id;
        let next = refusal
            .and_then(refusal_text)
            .map(|text| format!(" {text}, then restart the service."));
        let details = [
            code.map(|value| format!("exit code {value}")),
            signal.map(|value| format!("signal {value}")),
            reason.map(|value| format!("reason {value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        let args = crate::protocol::MessageSendArgs {
            max_model_requests: None,
            human: false,
            resume_recovery: false,

            sender_label: Some(format!("service:{id}")),
            thread_id: Some(self.thread),
            deliver: crate::protocol::Deliver::Next,
            text: format!(
                "Service {id} ended: execution {execution}, {state} ({details}). It will not restart automatically. Restart: shellboy service start --service {id}. Output: shellboy shell read --execution {execution}.{}",
                next.unwrap_or_default()
            ),
        };
        Ok(Some(store.send_owner_notice(
            tx,
            &args,
            &format!("service-ended:{execution}"),
            &super::state::now(),
        )?))
    }
}

fn disable_and_stop(
    tx: &Transaction<'_>,
    key: &str,
    by_thread: bool,
    removing: bool,
) -> Result<Vec<String>, StorageError> {
    let column = if by_thread { "thread_id" } else { "id" };
    tx.execute(
        &format!("UPDATE schedule SET enabled=0 WHERE kind='time' AND {column}=?1"),
        [key],
    )?;
    disable_services(tx, key, by_thread)?;
    let ids=tx.prepare(&format!("SELECT s.id FROM schedule q JOIN shell_execution s ON s.schedule_id=q.id WHERE q.{column}=?1 AND s.state IN ('starting','running','stopping')"))?.query_map([key],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let intent = if by_thread {
        ShellStopIntent::ThreadDeletion
    } else if !removing {
        ShellStopIntent::Explicit
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

fn shell_definition_command(command: &crate::protocol::Command) -> Option<(&str, &str, bool)> {
    use crate::protocol::Command;
    match command {
        Command::ScheduleRemove(args) => Some((&args.schedule_id, "time", true)),
        Command::ServiceRemove(args) => Some((&args.service_id, "service", true)),
        Command::ServiceStop(args) => Some((&args.service_id, "service", false)),
        _ => None,
    }
}

pub(super) async fn delete(
    control: &Arc<Control>,
    request: crate::protocol::Request,
) -> Result<serde_json::Value, ApiError> {
    use crate::protocol::{Command, ErrorCode};
    let command = request.command;
    let prepare = command.clone();
    // Removing a definition or Thread drops executions whose raw markers stay.
    let removes = shell_definition_command(&command).is_none_or(|(_, _, removing)| removing);
    let prepared = async {
        let publications = control.publications.clone();
        let (targets, executions) = control
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let (mut targets, by_thread, removing) = match &prepare {
                    Command::ThreadDelete(args) => {
                        (super::db::delete_targets(&tx, args)?, true, true)
                    }
                    Command::ThreadDeleteSet(args) => {
                        (super::db::delete_set_targets(&tx, args)?, true, true)
                    }
                    _ => {
                        let (id, kind, removing) =
                            shell_definition_command(&prepare).expect("definition operation");
                        let exists = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM schedule WHERE id=?1 AND kind=?2)",
                            params![id, kind],
                            |r| r.get::<_, bool>(0),
                        )?;
                        if !exists {
                            return Err(ApiError::new(
                                ErrorCode::TargetUnavailable,
                                "Shell definition not found",
                            )
                            .into());
                        }
                        (vec![id.to_owned()], false, removing)
                    }
                };
                targets.sort_unstable();
                let mut executions = Vec::new();
                for target in &targets {
                    executions.extend(disable_and_stop(&tx, target, by_thread, removing)?);
                }
                publications.commit_revoke(tx, executions.iter().map(String::as_str))?;
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
            if removes {
                crate::gc::request_marker_sweep_tx(tx)
                    .map_err(StorageError::from)
                    .map_err(super::storage_error)?;
            }
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
                Command::ThreadDeleteSet(args) => {
                    let current = super::db::delete_set_targets(tx, &args)?;
                    super::db::delete_targets_effects(tx, &current, &super::state::now())
                }
                definition => {
                    let (id, kind, removing) = shell_definition_command(&definition).expect("definition operation");
                    ensure_inactive(tx, id, false)?;
                    let sql = if removing {
                        "DELETE FROM schedule WHERE id=?1 AND kind=?2 RETURNING thread_id"
                    } else {
                        "SELECT thread_id FROM schedule WHERE id=?1 AND kind=?2"
                    };
                    let thread = tx.query_row(sql, params![id,kind], |r| r.get::<_, String>(0))
                        .optional().map_err(StorageError::from).map_err(super::storage_error)?;
                    let thread = match thread {
                        Some(thread) => thread,
                        None if kind == "time" => tx.query_row(
                            "SELECT s.detached_thread_id FROM json_each(?1) j CROSS JOIN shell_execution s ON s.id=j.value WHERE s.detached_thread_id IS NOT NULL LIMIT 1",
                            [serde_json::json!(executions).to_string()], |r| r.get::<_,String>(0),
                        ).optional().map_err(StorageError::from).map_err(super::storage_error)?.ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable,"Schedule not found"))?,
                        None => return Err(ApiError::new(ErrorCode::TargetUnavailable,"Service not found")),
                    };
                    let key = if kind == "time" { "schedule_id" } else { "service_id" };
                    let mut result = super::MutationResult::value(serde_json::json!({"accepted":true,(key):id}))?;
                    result.changed_threads.push(thread);
                    Ok(result)
                }
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
    if removes {
        signals.mark(crate::maintenance::Work::RawMarkers);
    }
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

    #[tokio::test]
    async fn runtime_audit_shutdown_cancels_reserved_thread_shells() {
        let (root, owner, control, accepted) =
            super::super::driver::preparation_tests::fixture("http://127.0.0.1:1/v1").await;
        let thread = accepted.thread_id;
        let saved_thread = thread.clone();
        owner.db.call(move |db| {
            for kind in ["time", "service"] {
                db.execute("INSERT INTO schedule(id,thread_id,kind,once_at,enabled,cwd,command) VALUES(?1,?2,?1,CASE WHEN ?1='time' THEN 1 END,1,'.','echo fixture')", params![kind,saved_thread])?;
                db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES(?1,?1,?1,'starting','.','{}','utf-8','utf-8','closed','2020-01-01T00:00:00Z')", [kind])?;
            }
            Ok(())
        }).await.unwrap();
        control.freeze_owner_admission();
        for kind in ["time", "service"] {
            spawn_reserved(
                &control,
                DueShell {
                    service: false,
                    id: kind.into(),
                    thread: thread.clone(),
                    cwd: root.path().into(),
                    command: "echo fixture".into(),
                },
            )
            .await
            .unwrap();
        }
        let results = owner.db.call(|db| {
            let mut query = db.prepare("SELECT id,state,end_reason,COALESCE(json_extract(shell_json,'$.spawned'),0) FROM shell_execution ORDER BY id")?;
            Ok(query.query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?,r.get::<_, Option<String>>(2)?,r.get::<_, bool>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        assert_eq!(
            results,
            ["service", "time"].map(|kind| (
                kind.to_owned(),
                "stopped".to_owned(),
                Some("owner_shutdown".to_owned()),
                false
            ))
        );
    }

    #[tokio::test]
    async fn runtime_audit_expired_disabled_one_shot_stays_disabled() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::storage::DbWorker::open(root.path().join("db"))
            .await
            .unwrap();
        db.call(|db| {
        let tx = db.transaction().unwrap();
        tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'); INSERT INTO schedule(id,thread_id,once_at,enabled,cwd,command) VALUES('one','t',1,0,'.','echo fixture');").unwrap();
        assert_eq!(change(&tx, "one", Change::Enabled(true)).err().unwrap().code, crate::protocol::ErrorCode::InvalidState);
        assert_eq!(list(&tx, "t", false).unwrap()["items"][0]["enabled"], false);
        Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn runtime_audit_latest_execution_survives_clock_rollback() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::storage::DbWorker::open(root.path().join("db"))
            .await
            .unwrap();
        db.call(|db| {
        let tx = db.transaction().unwrap();
        tx.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'); INSERT INTO schedule(id,thread_id,kind,enabled,cwd,command) VALUES('service','t','service',1,'.','echo fixture');").unwrap();
        for (id, state, time) in [("z-first", "failed", "2020-01-02T00:00:00Z"), ("a-second", "running", "2020-01-01T00:00:00Z")] {
            tx.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES(?1,'service',?1,?2,'.','{}','utf-8','utf-8','closed',?3)", params![id,state,time]).unwrap();
        }
        assert_eq!(list(&tx, "t", true).unwrap()["items"][0]["latest"]["execution_id"], "a-second");
        Ok(())
        }).await.unwrap();
        db.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn service_start_distinguishes_stopping_from_missing() {
        use crate::protocol::{ErrorCode, ServiceTarget};
        let (_root, owner, control, accepted) =
            super::super::driver::preparation_tests::fixture("http://127.0.0.1:1/v1").await;
        let thread = accepted.thread_id;
        owner.db.call(move |db| {
            db.execute("INSERT INTO schedule(id,thread_id,kind,enabled,cwd,command) VALUES('service',?1,'service',0,'.','echo fixture')", [&thread])?;
            db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('execution','service','execution','stopping','.','{}','utf-8','utf-8','closed','2020-01-01T00:00:00Z')", [])?;
            Ok(())
        }).await.unwrap();
        for (id, code) in [
            ("service", ErrorCode::InvalidState),
            ("missing", ErrorCode::TargetUnavailable),
        ] {
            let result = control
                .request(Request::new(
                    format!("{}:start-{id}", owner.operation_epoch.current()),
                    Command::ServiceStart(ServiceTarget {
                        service_id: id.into(),
                    }),
                ))
                .await;
            assert_eq!(result.error.unwrap().code, code);
        }
        owner
            .db
            .call(|db| {
                assert!(!db.query_row(
                    "SELECT enabled FROM schedule WHERE id='service'",
                    [],
                    |row| row.get::<_, bool>(0)
                )?);
                Ok(())
            })
            .await
            .unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

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
                format!("{}:delete", owner.operation_epoch.current()),
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
                    super::super::db::RuntimeReservation::New(_)
                ));
            }
            if point >= 2 {
                control
                    .store
                    .database
                    .call(|db| {
                        let tx = db.transaction()?;
                        assert_eq!(disable_and_stop(&tx, "target", true, true)?, ["x"]);
                        tx.commit()?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            let mut expected = None;
            if point == 3 {
                control.shell_start_intent("x", false, None).await.unwrap();
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
                    },
                )
                .await;
            shells.push(shell);
        }
        let targets = owner
            .db
            .call(|db| {
                let tx = db.transaction()?;
                let mut ids = disable_and_stop(&tx, "target", true, true)?;
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
        deletion_replay(false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn thread_delete_set_drains_selected_schedules_and_replays_once() {
        deletion_replay(true).await;
    }

    #[cfg(unix)]
    async fn deletion_replay(exact_set: bool) {
        let root = tempfile::tempdir().unwrap();
        let owner =
            crate::app::DataOwner::start(None, Some(root.path().join("data")), root.path().into())
                .await
                .unwrap();
        let control = Control::new(&owner, root.path().into());
        let cwd = root.path().to_string_lossy().into_owned();
        control.store.database.call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('target',?1,'','fixture','fixture','now')",[&cwd])?;
            if exact_set {
                for id in ["selected", "omitted"] {
                    db.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,'target',?2,'','fixture','fixture','now')", params![id, cwd])?;
                }
            }
            db.execute_batch("CREATE TEMP TABLE delete_witness(kind TEXT);
                CREATE TEMP TRIGGER stop_witness AFTER UPDATE OF state ON main.shell_execution WHEN NEW.state='stopping' AND OLD.state<>'stopping' BEGIN INSERT INTO delete_witness VALUES('stop'); END;
                CREATE TEMP TRIGGER delete_witness AFTER UPDATE OF deleted_at ON main.thread WHEN OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL BEGIN INSERT INTO delete_witness VALUES('delete'); END;
                CREATE TEMP TRIGGER gc_witness AFTER INSERT ON main.gc_job WHEN NEW.kind='history' BEGIN INSERT INTO delete_witness VALUES('gc'); END;
                CREATE TEMP TRIGGER receipt_witness BEFORE UPDATE OF state ON main.operation_receipt WHEN NEW.operation_id LIKE '%:delete' AND NEW.state='completed' BEGIN SELECT CASE WHEN (SELECT deleted_at FROM main.thread WHERE id='target') IS NULL OR EXISTS(SELECT 1 FROM main.schedule WHERE thread_id='target') THEN RAISE(ABORT,'delete receipt must match deletion') END; END;
                CREATE TEMP TRIGGER deletion_after_receipt AFTER UPDATE OF deleted_at ON main.thread WHEN NEW.id='target' AND NEW.deleted_at IS NOT NULL BEGIN SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM main.operation_receipt WHERE operation_id LIKE '%:delete' AND state='pending') THEN RAISE(ABORT,'deletion needs pending receipt') END; END;")?;
            Ok(())
        }).await.unwrap();
        control.start().await.unwrap();
        let (sender, requests) = tokio::sync::mpsc::channel(4);
        let server = crate::ipc::LocalServer::bind(&owner, sender).await.unwrap();
        let dispatcher = tokio::spawn(control.clone().serve(requests));
        let request = |id, command| {
            Request::new(format!("{}:{id}", owner.operation_epoch.current()), command)
        };
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
                    notify: Vec::new(),
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
        let command = if exact_set {
            let ids = vec!["target".into(), "selected".into()];
            let preview = control
                .request(request(
                    "preview",
                    Command::ThreadDeleteSetPreview(crate::protocol::ThreadDeleteSetArgs {
                        thread_ids: ids.clone(),
                    }),
                ))
                .await;
            assert!(preview.ok, "{:?}", preview.error);
            let preview = preview.result.unwrap();
            assert_eq!(preview["deletable_now"], true);
            assert_eq!(preview["targets"][1]["scheduled_execution_count"], 1);
            let marker: crate::protocol::DeletionPreviewMarker =
                serde_json::from_value(preview["preview_marker"].clone()).unwrap();
            let commit = |thread_ids| {
                Command::ThreadDeleteSet(crate::protocol::ThreadDeleteSetCommitArgs {
                    thread_ids,
                    preview_marker: marker.clone(),
                })
            };
            let stale = control
                .request(request(
                    "stale",
                    commit(vec!["target".into(), "omitted".into()]),
                ))
                .await;
            assert_eq!(
                stale.error.unwrap().code,
                crate::protocol::ErrorCode::PreviewStale
            );
            control.store.database.call(|db| {
                db.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('block','selected','live','now')", [])?;
                Ok(())
            }).await.unwrap();
            let blocked = control.request(request("blocked", commit(ids))).await;
            assert_eq!(
                blocked.error.unwrap().code,
                crate::protocol::ErrorCode::InvalidState
            );
            control
                .store
                .database
                .call(|db| {
                    assert_eq!(
                        db.query_row(
                            "SELECT state FROM shell_execution WHERE schedule_id IS NOT NULL",
                            [],
                            |row| row.get::<_, String>(0)
                        )?,
                        "running"
                    );
                    assert_eq!(
                        db.query_row("SELECT count(*) FROM delete_witness", [], |row| row
                            .get::<_, i64>(0))?,
                        0
                    );
                    db.execute("UPDATE run SET state='stopped' WHERE id='block'", [])?;
                    Ok(())
                })
                .await
                .unwrap();
            commit(vec!["selected".into(), "target".into()])
        } else {
            Command::ThreadDelete(crate::protocol::ThreadDeleteArgs {
                thread_id: "target".into(),
                descendants: false,
                preview_marker: None,
            })
        };
        let first = control.request(request("delete", command.clone())).await;
        let replay = control.request(request("delete", command)).await;
        assert!(first.ok, "{:?}", first.error);
        assert!(replay.ok, "{:?}", replay.error);
        assert_eq!(first.result, replay.result);
        control
            .store
            .database
            .call(move |db| {
                let facts = db
                    .prepare("SELECT kind FROM delete_witness ORDER BY rowid")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if exact_set {
                    assert_eq!(facts, ["stop", "delete", "delete", "gc"]);
                    assert!(db.query_row(
                        "SELECT deleted_at IS NULL FROM thread WHERE id='omitted'",
                        [],
                        |row| row.get::<_, bool>(0)
                    )?);
                } else {
                    assert_eq!(facts, ["stop", "delete", "gc"]);
                }
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

    /// #450: a definition removal and a real reservation that supersedes a
    /// collected run emit raw-marker work, and the maintenance pass for exactly
    /// the work they emitted (no explicit collect) settles what they left.
    #[tokio::test]
    async fn removal_and_supersession_emit_raw_marker_work_that_maintenance_settles() {
        let root = tempfile::tempdir().unwrap();
        let owner =
            crate::app::DataOwner::start(None, Some(root.path().join("data")), root.path().into())
                .await
                .unwrap();
        let control = Control::new(&owner, root.path().into());
        let cwd = root.path().to_string_lossy().into_owned();
        let collected =
            serde_json::json!({"reason":"raw_retention_age","state":"deleted","generation":1})
                .to_string();
        owner.db.call(move |db| {
            db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t',?1,'','p','m','now')",[&cwd])?;
            for (schedule, next_at) in [("removed", None), ("weekly", Some(1_i64))] {
                db.execute("INSERT INTO schedule(id,thread_id,cron,next_at,enabled,cwd,command) VALUES(?1,'t','0 0 * * 0',?2,1,?3,'true')",params![schedule,next_at,cwd])?;
                let execution = format!("{schedule}-run");
                db.execute("INSERT INTO shell_execution(id,schedule_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at) VALUES(?1,?2,?1,'exited',?3,'{}','utf-8','utf-8','closed','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z')",params![execution,schedule,cwd])?;
                db.execute("INSERT INTO app_meta(key,value) VALUES('raw_tombstone.'||?1,?2)",params![execution,collected])?;
            }
            Ok(())
        }).await.unwrap();
        let signals = owner.db.signals().clone();
        // A maintenance pass for exactly the kind a producer emitted; the
        // owner's running loop may take the mark first, so it is read from
        // the producer record and the pending work from the database.
        let maintain = || {
            assert!(
                signals
                    .take_seen()
                    .contains(crate::maintenance::Work::RawMarkers)
            );
            let mut set = crate::maintenance::WorkSet::default();
            set.insert(crate::maintenance::Work::RawMarkers);
            control.gc.collect_kinds(set, Utc::now())
        };
        // The owner's first marker pass (its startup sweep) settles nothing
        // here: both runs are still their definition's latest.
        signals.mark(crate::maintenance::Work::RawMarkers);
        maintain().await.unwrap();
        let removed = control
            .request(Request::new(
                format!(
                    "{}:{}",
                    owner.operation_epoch.current(),
                    uuid::Uuid::new_v4()
                ),
                Command::ScheduleRemove(crate::protocol::ScheduleTarget {
                    schedule_id: "removed".into(),
                }),
            ))
            .await;
        assert!(removed.ok, "{:?}", removed.error);
        maintain().await.unwrap();
        // The weekly run is still its definition's latest; the real reservation
        // supersedes it after its output was already collected.
        dispatch(&control).await.unwrap();
        maintain().await.unwrap();
        owner.db.call(|db| {
            let markers: Vec<String> = db.prepare("SELECT key FROM app_meta WHERE key>='raw_tombstone.' AND key<'raw_tombstone/'")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            assert!(markers.is_empty(), "orphan and superseded markers are settled: {markers:?}");
            let weekly: Vec<String> = db.prepare("SELECT id FROM shell_execution WHERE schedule_id='weekly'")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            assert!(!weekly.iter().any(|id| id == "weekly-run"), "superseded collected run is deleted");
            assert!(!weekly.is_empty(), "the new latest run stays");
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();
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
                format!(
                    "{}:{}",
                    owner.operation_epoch.current(),
                    uuid::Uuid::new_v4()
                ),
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
            assert!(
                control
                    .shell_start_intent("x", false, None)
                    .await
                    .unwrap()
                    .0
            );
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
                    format!("{}:get", owner.operation_epoch.current()),
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
    async fn service_exit_records_one_message_and_disables_the_service() {
        check_service_failure(ServiceFailure::Exit).await;
    }

    #[tokio::test]
    async fn service_spawn_failure_records_one_message_and_disables_the_service() {
        check_service_failure(ServiceFailure::SpawnFailed).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn service_refused_for_descriptor_headroom_is_saved_failed_with_the_typed_reason() {
        check_service_failure(ServiceFailure::DescriptorBudget).await;
    }

    #[derive(Clone, Copy, PartialEq)]
    enum ServiceFailure {
        Exit,
        SpawnFailed,
        #[cfg(unix)]
        DescriptorBudget,
    }

    async fn check_service_failure(failure: ServiceFailure) {
        let spawn_failure = failure != ServiceFailure::Exit;
        use crate::{
            app::DataOwner,
            protocol::{ExecutionTarget, MessageSendArgs, ServiceAddArgs, ThreadTarget},
        };

        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n[limits]\ndraft_bytes=1\n").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let request = |command| {
            Request::new(
                format!(
                    "{}:{}",
                    owner.operation_epoch.current(),
                    uuid::Uuid::new_v4()
                ),
                command,
            )
        };
        let create = control
            .request(request(Command::ThreadCreate(
                crate::protocol::ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    cwd: None,
                    model: None,
                    effort: None,
                    parent_thread_id: None,
                },
            )))
            .await;
        assert!(create.ok, "{:?}", create.error);
        let thread = create.result.unwrap()["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let external = control
            .request(request(Command::MessageSend(MessageSendArgs {
                max_model_requests: None,
                human: false,
                resume_recovery: false,

                sender_label: None,
                thread_id: Some(thread.clone()),
                deliver: crate::protocol::Deliver::Next,
                text: "too long".into(),
            })))
            .await;
        assert_eq!(
            external.error.unwrap().code,
            crate::protocol::ErrorCode::InputLimit
        );
        let added = control
            .request(request(Command::ServiceAdd(ServiceAddArgs {
                name: None,
                thread_id: Some(thread.clone()),
                cwd: (failure == ServiceFailure::SpawnFailed).then(|| "missing-directory".into()),
                description: None,
                command: "exit 3".into(),
            })))
            .await;
        assert!(added.ok, "{:?}", added.error);
        // The exit case starts exactly at the headroom boundary; one more open
        // descriptor is refused. The fixture replaces only the live observation.
        #[cfg(unix)]
        {
            use crate::shell::descriptor_budget::{DescriptorUse, IPC_RESERVE, SPAWN_ADD};
            let limit = 1024;
            let boundary = limit - SPAWN_ADD - IPC_RESERVE;
            let open = match failure {
                ServiceFailure::DescriptorBudget => Some(boundary + 1),
                ServiceFailure::Exit => Some(boundary),
                ServiceFailure::SpawnFailed => None,
            };
            control
                .shell
                .set_descriptor_fixture(open.map(|open| DescriptorUse { open, limit }));
        }
        dispatch(&control).await.unwrap();
        let service = added.result.unwrap()["service_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let execution = crate::test_watchdog::watchdog("service exits", async {
            loop {
                let changed = control.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let listed = control
                    .request(request(Command::ServiceList(ThreadTarget {
                        thread_id: Some(thread.clone()),
                    })))
                    .await;
                assert!(listed.ok, "{:?}", listed.error);
                let body = listed.result.unwrap();
                let items = body["items"].as_array().unwrap();
                assert_eq!(items.len(), 1);
                if items[0]["enabled"] == false {
                    break items[0]["latest"]["execution_id"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                }
                changed.await;
            }
        })
        .await
        .unwrap();
        let saved = control
            .request(request(Command::ShellGet(ExecutionTarget {
                execution_id: execution.clone(),
            })))
            .await
            .result
            .unwrap();
        if spawn_failure {
            assert_eq!(saved["state"], "failed");
            let reason = match failure {
                #[cfg(unix)]
                ServiceFailure::DescriptorBudget => "descriptor_budget",
                _ => "spawn_failed",
            };
            assert_eq!(saved["reason"], reason);
        } else {
            assert_eq!(saved["state"], "exited");
            assert_eq!(saved["exit_code"], 3);
        }
        let operation = format!("service-ended:{execution}");
        dispatch(&control).await.unwrap();
        let (executions, messages, assigned_turns) = owner.db.call(move |db| {
            let executions: i64 = db.query_row(
                "SELECT count(*) FROM shell_execution WHERE schedule_id=?1", [&service], |row| row.get(0),
            )?;
            let (messages, assigned_turns): (i64, i64) = db.query_row(
                "SELECT count(*),count(turn_id) FROM receipt WHERE client_operation_id=?1 AND target_thread_id=?2",
                [operation, thread], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok((executions, messages, assigned_turns))
        }).await.unwrap();
        assert_eq!((executions, messages, assigned_turns), (1, 1, 1));
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn service_restarts_after_owner_shutdown_but_not_after_stop() {
        use crate::{
            app::DataOwner,
            protocol::{ServiceAddArgs, ServiceTarget, ThreadTarget},
        };
        async fn call(control: &Arc<Control>, command: Command) -> serde_json::Value {
            let result = control
                .request(Request::new(
                    format!(
                        "{}:{}",
                        control.store.operation_epoch.current(),
                        uuid::Uuid::new_v4()
                    ),
                    command,
                ))
                .await;
            assert!(result.ok, "{:?}", result.error);
            result.result.unwrap()
        }
        async fn row(control: &Arc<Control>, thread: &str) -> serde_json::Value {
            call(
                control,
                Command::ServiceList(ThreadTarget {
                    thread_id: Some(thread.into()),
                }),
            )
            .await["items"][0]
                .clone()
        }
        let (root, owner, control, accepted) =
            super::super::driver::preparation_tests::fixture("http://127.0.0.1:1/v1").await;
        let thread = accepted.thread_id;
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 600"
        } else {
            "exec sleep 600"
        };
        let service = call(
            &control,
            Command::ServiceAdd(ServiceAddArgs {
                name: None,
                thread_id: Some(thread.clone()),
                cwd: None,
                description: None,
                command: command.into(),
            }),
        )
        .await["service_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let store = control.store.clone();
        let reserved = owner
            .db
            .call(move |db| {
                let tx = db.transaction()?;
                let reserved = advance_due(&store, &tx, Utc::now(), "native")?
                    .shell
                    .unwrap();
                tx.commit()?;
                Ok(reserved)
            })
            .await
            .unwrap();
        spawn_reserved(&control, reserved).await.unwrap();
        let first = row(&control, &thread).await;
        assert_eq!(first["latest"]["state"], "running");
        assert_eq!(next_deadline(&control).await.unwrap(), None);
        let first_id = first["latest"]["execution_id"].as_str().unwrap().to_owned();
        // TUI Quit closes admission before shutting down the running service.
        control.close_tool_start_admission();
        control.shutdown().await.unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
        let owner = DataOwner::start(
            Some(root.path().join("config.toml")),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        dispatch(&control).await.unwrap();
        let restarted = row(&control, &thread).await;
        assert_eq!(restarted["enabled"], true);
        assert_eq!(restarted["latest"]["state"], "running");
        let execution = restarted["latest"]["execution_id"].as_str().unwrap();
        let details = call(
            &control,
            Command::ShellGet(crate::protocol::ExecutionTarget {
                execution_id: execution.into(),
            }),
        )
        .await;
        assert_eq!(details["foreground"], false);
        let list = call(
            &control,
            Command::ShellList(
                serde_json::from_value(json!({"thread_id":thread,"state":"running"})).unwrap(),
            ),
        )
        .await;
        let service_row = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["execution_id"] == execution)
            .unwrap();
        assert_eq!(service_row["foreground"], false);
        assert_ne!(restarted["latest"]["execution_id"], first_id);
        assert!(control.live.clone_shell_handle(&first_id).await.is_none());
        call(
            &control,
            Command::ServiceStop(ServiceTarget {
                service_id: service.clone(),
            }),
        )
        .await;
        let stopped = row(&control, &thread).await;
        assert_eq!(stopped["enabled"], false);
        assert_eq!(stopped["latest"]["state"], "stopped");
        dispatch(&control).await.unwrap();
        assert_eq!(row(&control, &thread).await, stopped);
        let target = service.clone();
        owner.db.call(move |db| {
            let count: i64 = db.query_row("SELECT count(*) FROM receipt WHERE client_operation_id IN (SELECT 'service-ended:'||id FROM shell_execution WHERE schedule_id=?1)", [target], |r| r.get(0))?;
            assert_eq!(count, 0);
            Ok(())
        }).await.unwrap();
        call(
            &control,
            Command::ServiceStart(ServiceTarget {
                service_id: service.clone(),
            }),
        )
        .await;
        dispatch(&control).await.unwrap();
        assert_eq!(row(&control, &thread).await["latest"]["state"], "running");
        call(
            &control,
            Command::ServiceRemove(ServiceTarget {
                service_id: service,
            }),
        )
        .await;
        assert!(row(&control, &thread).await.is_null());
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn skipped_occurrences_each_reach_the_model_only_when_notify_is_set() {
        use super::super::shell_lifecycle_tests::{answer, fixture, incoming, rpc};
        use crate::{
            app::DataOwner,
            protocol::{ScheduleAddArgs, ScheduleSetArgs, ThreadCreateArgs, ThreadTarget},
        };
        async fn listed(control: &Arc<Control>, thread: &str) -> Vec<serde_json::Value> {
            rpc(
                control,
                Command::ScheduleList(ThreadTarget {
                    thread_id: Some(thread.into()),
                }),
            )
            .await["items"]
                .as_array()
                .unwrap()
                .clone()
        }
        // Arm every timed schedule's next occurrence at the same past time and
        // dispatch each due one through the normal path.
        async fn occur(control: &Arc<Control>, owner: &DataOwner, at: i64) {
            owner
                .db
                .call(move |db| {
                    db.execute("UPDATE schedule SET next_at=?1 WHERE kind='time'", [at])?;
                    Ok(())
                })
                .await
                .unwrap();
            for _ in 0..2 {
                dispatch(control).await.unwrap();
            }
        }
        // (client_operation_id, saved message ID) of each notice for the schedule.
        async fn notices(owner: &DataOwner, schedule: &str) -> Vec<(String, String)> {
            let prefix = format!("schedule-skipped:{schedule}:");
            owner.db.call(move |db| {
                let mut query = db.prepare("SELECT client_operation_id,message_node_id FROM receipt WHERE substr(client_operation_id,1,length(?1))=?1 ORDER BY client_operation_id")?;
                Ok(query.query_map([prefix], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)
            }).await.unwrap()
        }
        fn texts(value: &serde_json::Value, out: &mut Vec<String>) {
            match value {
                serde_json::Value::String(text) => out.push(text.clone()),
                serde_json::Value::Array(items) => items.iter().for_each(|item| texts(item, out)),
                serde_json::Value::Object(map) => map.values().for_each(|item| texts(item, out)),
                _ => {}
            }
        }
        // The model request carries an input naming this schedule, skipped
        // time and still-active execution.
        fn carries(request: &serde_json::Value, schedule: &str, at: i64, execution: &str) -> bool {
            let at = DateTime::from_timestamp(at, 0).unwrap().to_rfc3339();
            let mut found = Vec::new();
            texts(&request["input"], &mut found);
            found.iter().any(|text| {
                text.contains(schedule) && text.contains(&at) && text.contains(execution)
            })
        }
        let (_root, owner, control, mut requests, server) = fixture().await;
        let thread = rpc(
            &control,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                model: None,
                effort: None,
                cwd: None,
                parent_thread_id: None,
            }),
        )
        .await["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut ids = Vec::new();
        for notify in [vec![ScheduleNotify::Skipped], Vec::new()] {
            let added = rpc(
                &control,
                Command::ScheduleAdd(ScheduleAddArgs {
                    thread_id: Some(thread.clone()),
                    cron: Some("0 0 1 1 *".into()),
                    at: None,
                    in_seconds: None,
                    cwd: None,
                    description: None,
                    notify,
                    command: "exec sleep 600".into(),
                }),
            )
            .await;
            ids.push(added["schedule_id"].as_str().unwrap().to_owned());
        }
        let (notified, silent) = (ids[0].clone(), ids[1].clone());
        let item = |items: &[serde_json::Value], id: &str| {
            items
                .iter()
                .find(|item| item["schedule_id"] == id)
                .unwrap()
                .clone()
        };
        let items = listed(&control, &thread).await;
        assert_eq!(item(&items, &notified)["notify"], json!(["skipped"]));
        assert_eq!(item(&items, &silent)["notify"], json!([]));

        // The first occurrence starts both; later ones find both still active.
        occur(&control, &owner, 60).await;
        let running = listed(&control, &thread).await;
        let execution = item(&running, &notified)["latest"]["execution_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let silent_execution = item(&running, &silent)["latest"]["execution_id"].clone();
        for (at, nth) in [(120, 1), (180, 2)] {
            occur(&control, &owner, at).await;
            let items = listed(&control, &thread).await;
            for id in [&notified, &silent] {
                let listed = item(&items, id);
                assert_eq!(
                    listed["last_skipped_at"],
                    DateTime::from_timestamp(at, 0).unwrap().to_rfc3339()
                );
            }
            assert_eq!(item(&items, &notified)["latest"]["execution_id"], execution);
            assert_eq!(
                item(&items, &silent)["latest"]["execution_id"],
                silent_execution
            );
            // Each distinct skip of the same execution reaches the model.
            let (request, reply) = incoming(&mut requests).await;
            assert!(carries(&request, &notified, at, &execution), "{request}");
            reply.send(answer()).unwrap();
            assert_eq!(notices(&owner, &notified).await.len(), nth);
        }
        assert!(notices(&owner, &silent).await.is_empty());
        let sent = notices(&owner, &notified).await;
        assert_eq!(
            sent.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
            [120, 180].map(|at| format!("schedule-skipped:{notified}:{at}"))
        );
        for (_, message) in &sent {
            let saved = rpc(
                &control,
                Command::MessageGet(crate::protocol::MessageTarget {
                    message_id: message.clone(),
                }),
            )
            .await;
            assert_eq!(saved["thread_id"], thread);
            assert_eq!(saved["sender"]["label"], format!("schedule:{notified}"));
        }

        // Meeting an already noticed occurrence again sends nothing new.
        occur(&control, &owner, 180).await;
        assert_eq!(notices(&owner, &notified).await, sent);

        // Clearing the setting silences later skips.
        rpc(
            &control,
            Command::ScheduleSet(ScheduleSetArgs {
                schedule_id: notified.clone(),
                description: None,
                notify: Some(vec![ScheduleNotify::None]),
            }),
        )
        .await;
        assert_eq!(
            item(&listed(&control, &thread).await, &notified)["notify"],
            json!([])
        );
        occur(&control, &owner, 240).await;
        assert_eq!(notices(&owner, &notified).await, sent);
        control.close_tool_start_admission();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn one_shot_refused_for_descriptor_headroom_is_retired_with_one_notice() {
        use crate::protocol::{ExecutionTarget, ScheduleAddArgs, ShellListArgs, ThreadTarget};
        use crate::shell::descriptor_budget::{DescriptorUse, IPC_RESERVE, SPAWN_ADD};

        let (_root, owner, control, accepted) =
            super::super::driver::preparation_tests::fixture("http://127.0.0.1:1/v1").await;
        let thread = accepted.thread_id;
        let request = |command| Request::new(owner.operation_epoch.mint(), command);
        let add = control
            .request(request(Command::ScheduleAdd(ScheduleAddArgs {
                thread_id: Some(thread.clone()),
                cron: None,
                at: None,
                in_seconds: Some(1),
                cwd: None,
                description: None,
                notify: Vec::new(),
                command: "echo must-not-run".into(),
            })))
            .await;
        assert!(add.ok, "{:?}", add.error);
        let schedule = add.result.unwrap()["schedule_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let key = schedule.clone();
        owner
            .db
            .call(move |db| {
                db.execute("UPDATE schedule SET next_at=1,once_at=1 WHERE id=?1", [key])?;
                Ok(())
            })
            .await
            .unwrap();
        let limit = 1024;
        control.shell.set_descriptor_fixture(Some(DescriptorUse {
            open: limit - SPAWN_ADD - IPC_RESERVE + 1,
            limit,
        }));
        dispatch(&control).await.unwrap();
        let listed = control
            .request(request(Command::ShellList(
                serde_json::from_value::<ShellListArgs>(json!({"thread_id":thread})).unwrap(),
            )))
            .await
            .result
            .unwrap();
        let execution = listed["items"][0]["execution_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let saved = control
            .request(request(Command::ShellGet(ExecutionTarget {
                execution_id: execution.clone(),
            })))
            .await
            .result
            .unwrap();
        assert_eq!(saved["state"], "failed");
        assert_eq!(saved["reason"], "descriptor_budget");
        let remaining = control
            .request(request(Command::ScheduleList(ThreadTarget {
                thread_id: Some(thread.clone()),
            })))
            .await
            .result
            .unwrap();
        assert!(remaining["items"].as_array().unwrap().is_empty());
        let operation = format!("schedule-refused:{execution}");
        let notices: i64 = owner
            .db
            .call(move |db| {
                Ok(db.query_row(
                "SELECT count(*) FROM receipt WHERE client_operation_id=?1 AND target_thread_id=?2",
                [operation, thread], |row| row.get(0),
            )?)
            })
            .await
            .unwrap();
        assert_eq!(notices, 1);
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
                format!(
                    "{}:{}",
                    owner.operation_epoch.current(),
                    uuid::Uuid::new_v4()
                ),
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
                    notify: Vec::new(),
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
                assert_eq!(active["foreground"], false);
                assert_eq!(
                    control.request(request(get())).await.result.unwrap()["foreground"],
                    false
                );
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
            assert!(saved.get("foreground").is_none());
            let history = control.request(request(list())).await.result.unwrap();
            let ended = history["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["execution_id"] == execution)
                .unwrap();
            assert!(ended.get("foreground").is_none());
            if expected == "exited" {
                assert_eq!(
                    saved["exit_code"],
                    if command.ends_with("exit 7") { 7 } else { 0 }
                );
                let read = control
                    .request(request(Command::ShellRead(ShellReadArgs {
                        expected_owner: None,
                        tail: false,
                        segments: false,
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
