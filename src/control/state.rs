//! Persistent state changes, executed on the existing caller DB job/transaction.
//! Terminal updates preserve settled audit data and classify zero-row outcomes.
use crate::{
    protocol::{ApiError, AttemptState, ErrorCode, RunState, TurnState},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
mod interrupted_tools;
pub(crate) use interrupted_tools::{RepairReport, repair_interrupted_tool_groups};

pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Create the Turn and its existing context observation in the caller's
/// transaction. Admission predicates and wake/recovery policy stay with callers.
pub(super) fn create_turn_tx(
    tx: &rusqlite::Connection,
    thread: &str,
    run: &str,
    created_at: &str,
    started_at: &str,
) -> rusqlite::Result<String> {
    let turn = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES(?1,?2,?3,'preparing',?4)",
        params![turn, thread, run, created_at],
    )?;
    super::context::turn_started(tx, thread, run, &turn, started_at)?;
    Ok(turn)
}

/// Resume one observed recovery Turn and only its held recovery input.
/// Authorization, context actor and wake policy remain at each admission caller.
pub(super) fn resume_recovery_turn_tx(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    generation: i64,
    at: &str,
) -> Result<bool, StorageError> {
    use super::usage_limit_retry as usage;
    let reason: Option<Option<String>> = tx
        .query_row(
            "SELECT recovery_reason_json FROM turn WHERE id=?1 AND thread_id=?2 AND run_id=?3
         AND generation=?4 AND state='recovery_wait'
         AND EXISTS(SELECT 1 FROM run r WHERE r.id=turn.run_id AND r.state='live')",
            params![turn, thread, run, generation],
            |row| row.get(0),
        )
        .optional()?;
    let Some(reason) = reason else {
        return Ok(false);
    };
    let mut parsed: Option<Value> = reason
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok());
    let consumed_now = parsed.as_mut().is_some_and(|reason| {
        let Some(reset) = usage::reset(reason) else {
            return false;
        };
        usage::mark_consumed(reason, reset.timestamp());
        true
    });
    super::message_delivery::recover_on_resume_tx(
        tx,
        thread,
        run,
        turn,
        json!({"kind":"shellboy"}),
        at,
    )?;
    let remaining: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='held' AND turn_id=?2 AND run_id=?3 AND hold_reason='recovery_wait')", params![thread,turn,run], |r| r.get(0))?;
    // One admission drains one bounded unit and commits its reset with the transition.
    let (state, reason) =
        if remaining || super::message_delivery::interrupted_remaining(tx, thread)? {
            (
                "recovery_wait",
                if consumed_now {
                    parsed.map(|value| value.to_string())
                } else {
                    reason
                },
            )
        } else {
            (
                "preparing",
                parsed
                    .as_ref()
                    .and_then(usage::consumed)
                    .map(usage::consumed_only),
            )
        };
    let changed = tx.execute(
        "UPDATE turn SET state=?2,generation=generation+1,recovery_reason_json=?3
         WHERE id=?1 AND generation=?4 AND state='recovery_wait'",
        params![turn, state, reason, generation],
    )?;
    // The caller rolls back the receipt batch if the transition writes no row.
    Ok(changed == 1)
}

/// A DB transition result, not a second in-memory attempt state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptTransition {
    Applied,
    AlreadyApplied,
    Obsolete,
}

pub(super) fn adopt_attempt(
    db: &Connection,
    attempt: &str,
) -> Result<AttemptTransition, StorageError> {
    let adopted: Option<String> = db.query_row(
        "UPDATE model_attempt SET input_manifest_json=json_set(input_manifest_json,'$.adoption',json_object('outcome','adopted'))
         WHERE id=?1 AND kind='conversation' AND (state='completed' OR (state='incomplete' AND json_extract(error_json,'$.error.kind')='output_limit')) AND json_type(input_manifest_json,'$.adoption') IS NULL
         AND EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id
                    WHERE u.id=model_attempt.turn_id AND u.generation=model_attempt.generation
                    AND u.state='running' AND r.state='live') RETURNING state",
        [attempt], |row| row.get(0),
    ).optional()?;
    if adopted.as_deref() == Some("completed") {
        let evidence:(String,String,String,Option<String>,Option<i64>)=db.query_row(
            "SELECT t.thread_id,a.profile_id,a.model_id,CASE WHEN json_type(a.input_manifest_json,'$.compaction_report_token') IS NULL THEN NULL WHEN json_type(a.input_manifest_json,'$.compaction_report_token')='object' THEN json_extract(a.input_manifest_json,'$.compaction_report_token') ELSE 'invalid-type' END, json_extract(a.usage_json,'$.input_tokens') FROM model_attempt a JOIN turn t ON t.id=a.turn_id WHERE a.id=?1",
            [attempt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        crate::compaction::a_state::complete_report_tx(
            db,
            &evidence.0,
            &evidence.1,
            &evidence.2,
            attempt,
            evidence
                .3
                .as_deref()
                .map(|json| {
                    serde_json::from_str::<crate::compaction::a_state::Token>(json)
                        .map_err(|e| crate::storage::StorageError::Schema(e.to_string()))
                })
                .transpose()?,
            evidence.4.and_then(|input| u64::try_from(input).ok()),
        )?;
    }
    if adopted.is_some() {
        return Ok(AttemptTransition::Applied);
    }
    let row = db.query_row("SELECT a.state,a.generation,u.generation,u.state,r.state FROM model_attempt a JOIN turn u ON u.id=a.turn_id JOIN run r ON r.id=u.run_id WHERE a.id=?1", [attempt], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?))).optional()?;
    let Some((state, generation, current, turn, run)) = row else {
        return Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            "Model attempt is no longer available",
        )
        .into());
    };
    let state: AttemptState = serde_json::from_value(Value::String(state))
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved model attempt state"))?;
    if generation != current || run != "live" {
        return Ok(AttemptTransition::Obsolete);
    }
    if state == AttemptState::Completed {
        return Ok(AttemptTransition::AlreadyApplied);
    }
    if turn != "running"
        || matches!(
            state,
            AttemptState::Failed | AttemptState::Incomplete | AttemptState::Cancelled
        )
    {
        return Ok(AttemptTransition::Obsolete);
    }
    Err(if state == AttemptState::Inflight {
        ApiError::new(
            ErrorCode::OperationConflict,
            "Model attempt was not changed at its expected boundary",
        )
    } else {
        ApiError::new(
            ErrorCode::InvalidState,
            "Model attempt predecessor does not permit this transition",
        )
    }
    .into())
}
pub(super) fn cancel_run_attempts(
    db: &Connection,
    run: &str,
    at: &str,
) -> Result<usize, StorageError> {
    Ok(db.execute("UPDATE model_attempt SET state='cancelled',ended_at=?2 WHERE turn_id IN (SELECT id FROM turn WHERE run_id=?1) AND state IN ('preparing','inflight')",params![run,at])?)
}
pub(crate) fn interrupt_previous_attempts(
    db: &Connection,
    at: &str,
) -> Result<usize, StorageError> {
    Ok(db.execute("UPDATE model_attempt SET state='incomplete',ended_at=?1,error_json=?2 WHERE state IN ('preparing','inflight')",params![at,r#"{"kind":"interrupted","message":"Previous instance ended; server-side results may be uncertain"}"#])?)
}

pub(super) fn recover_turn_tx(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    generation: i64,
    error: &ApiError,
) -> Result<bool, crate::storage::StorageError> {
    let mut reason = error.clone();
    let mut details = match reason.details.take() {
        None => serde_json::Map::new(),
        Some(Value::Object(details)) => details,
        Some(original) => serde_json::Map::from_iter([("original_details".into(), original)]),
    };
    details.insert("last_failure_at".into(), Value::String(now()));
    reason.details = Some(Value::Object(details));
    let mut reason = serde_json::to_value(&reason).expect("recovery reason serializes");
    if let Some(at) = super::usage_limit_retry::retained_reset(tx, turn)? {
        super::usage_limit_retry::mark_consumed(&mut reason, at);
    }
    let changed=tx.execute("UPDATE turn SET state='recovery_wait',recovery_reason_json=?2 WHERE id=?1 AND generation=?3 AND state IN ('preparing','running','waiting')",params![turn,reason.to_string(),generation])?;
    if changed > 0 {
        tx.execute(
            "UPDATE attachment_delivery SET status='held' WHERE thread_id=?1 AND status='accepted'",
            [thread],
        )?;
        super::revisit::hand_off_tx(tx, Some(thread), "recovery_wait", Some((run, turn)), &now())?;
        record_recovery_event(
            tx,
            run,
            &format!("recovery-{turn}-{generation}"),
            "recovery_wait",
            json!({"thread_id":thread,"run_id":run,"turn_id":turn,"generation":generation,"state":"recovery_wait","error":reason}),
        )?;
    }
    Ok(changed > 0)
}

pub(super) fn record_recovery_event(
    tx: &rusqlite::Transaction<'_>,
    child_run: &str,
    event_id: &str,
    kind: &str,
    payload: Value,
) -> Result<(), crate::storage::StorageError> {
    if kind == "recovery_wait"
        && let Some(thread) = payload.get("thread_id").and_then(Value::as_str)
    {
        tx.execute("INSERT OR IGNORE INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at) VALUES(?1,'recovery_wait',?2,?3,?4,?5)",params![event_id,json!({"run_id":child_run}).to_string(),thread,payload.to_string(),now()])?;
    }
    Ok(())
}

/// next() has checked that no Turn, pending input, or owned live work remains
/// in this same transaction; preserve its normal completion notification.
pub(super) fn complete_idle_run(
    tx: &rusqlite::Transaction<'_>,
    run: &str,
) -> Result<(), StorageError> {
    let changed = tx.execute(
        "UPDATE run SET state='completed',ended_at=?2 WHERE id=?1 AND state='live'",
        params![run, now()],
    )?;
    if changed == 0 {
        let saved: Option<String> = tx
            .query_row("SELECT state FROM run WHERE id=?1", [run], |r| r.get(0))
            .optional()?;
        let saved = saved
            .ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable, "Run no longer exists"))?;
        let saved: RunState = serde_json::from_value(Value::String(saved))
            .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved Run state"))?;
        return if saved == RunState::Live {
            Err(ApiError::new(
                ErrorCode::OperationConflict,
                "Run did not complete at its expected boundary",
            )
            .into())
        } else {
            Ok(()) // Settled completion or obsolete stopping/terminal ownership.
        };
    }

    Ok(())
}

/// A normal completed response with no calls is saved in this transaction.
/// New accepted input keeps this Turn open for the following request.
pub(super) fn complete_answer_turn(
    tx: &rusqlite::Transaction<'_>,
    turn: &str,
    node: &str,
) -> Result<bool, StorageError> {
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM receipt WHERE turn_id=?1 AND state='accepted')",
        [&turn],
        |r| r.get(0),
    )?;
    if !pending {
        let changed = tx.execute(
            "UPDATE turn SET state='completed',answer_node_id=?2,ended_at=?3 WHERE id=?1 AND state='running' AND EXISTS(SELECT 1 FROM run r WHERE r.id=turn.run_id AND r.state='live')",
            params![turn, node, now()],
        )?;
        if changed == 0 {
            let saved = tx.query_row("SELECT u.state,u.answer_node_id,r.state FROM turn u JOIN run r ON r.id=u.run_id WHERE u.id=?1",[turn],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?))).optional()?.ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable,"Turn no longer exists"))?;
            let state: TurnState = serde_json::from_value(Value::String(saved.0))
                .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved Turn state"))?;
            let owner: RunState = serde_json::from_value(Value::String(saved.2))
                .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved Run state"))?;
            if state == TurnState::Completed {
                return if saved.1.as_deref() == Some(node) {
                    Ok(false)
                } else {
                    Err(ApiError::new(
                        ErrorCode::OperationConflict,
                        "Turn already has a different answer",
                    )
                    .into())
                };
            }
            if matches!(state, TurnState::Stopped | TurnState::Failed) || owner != RunState::Live {
                return Ok(false); // Obsolete completion cannot revive a settled Turn.
            }
            let code = if state == TurnState::Running {
                ErrorCode::OperationConflict
            } else {
                ErrorCode::InvalidState
            };
            return Err(
                ApiError::new(code, "Turn did not complete at its expected boundary").into(),
            );
        }
        return Ok(true);
    }
    Ok(false)
}

/// Ownership has already been frozen, and every live handle has drained.
pub(super) enum RunStopAdmission<'a> {
    Explicit { at: &'a str },
    DriverDrain,
}

/// Fence in-flight results before releasing this transaction to the drain.
pub(super) fn admit_run_stop_tx(
    tx: &rusqlite::Transaction<'_>,
    run: &str,
    admission: RunStopAdmission<'_>,
) -> Result<(), StorageError> {
    let (reason, hold_at) = match admission {
        RunStopAdmission::Explicit { at } => (Some("explicit_stop"), Some(at)),
        // Drain preserves a prior explicit reason; accepted receipts are settled
        // at the terminal boundary, not by a second explicit admission.
        RunStopAdmission::DriverDrain => (None, None),
    };
    tx.execute("UPDATE run SET state='stopping',stop_reason=COALESCE(?2,stop_reason) WHERE id=?1 AND state='live'",params![run,reason])?;
    tx.execute("UPDATE turn SET generation=generation+1 WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",[run])?;
    super::attachment_delivery::hold_run_tx(tx, run)?;
    if let Some(at) = hold_at {
        tx.execute("UPDATE receipt SET state='held',hold_reason='stopping',updated_at=?2 WHERE run_id=?1 AND state='accepted'",params![run,at])?;
    }
    Ok(())
}

pub(super) fn stop_drained_runs(
    tx: &rusqlite::Transaction<'_>,
    owned: &[(String, String)],
    stopping_for_shutdown: bool,
    keys: &mut Vec<super::wait::wakes::Key>,
) -> Result<(), StorageError> {
    stop_drained_runs_inner(tx, owned, stopping_for_shutdown, None, keys)
}

/// Exact Run stop settlement; unlike startup/owner shutdown this repairs only
/// the Thread whose Run is being stopped.
pub(super) fn stop_drained_run_tx(
    tx: &rusqlite::Transaction<'_>,
    run: &str,
    thread: &str,
    keys: &mut Vec<super::wait::wakes::Key>,
) -> Result<(), StorageError> {
    stop_drained_runs_inner(
        tx,
        &[(run.to_owned(), thread.to_owned())],
        false,
        Some(thread),
        keys,
    )
}

fn stop_drained_runs_inner(
    tx: &rusqlite::Transaction<'_>,
    owned: &[(String, String)],
    stopping_for_shutdown: bool,
    repair_thread: Option<&str>,
    keys: &mut Vec<super::wait::wakes::Key>,
) -> Result<(), StorageError> {
    use super::wait::wakes::Key;
    for (run, thread) in owned {
        // The same held input a send to this stopping Run would have produced.
        super::revisit::hand_off_tx(
            tx,
            Some(thread),
            if stopping_for_shutdown {
                "previous_instance_interrupted"
            } else {
                "stopping"
            },
            None,
            &now(),
        )?;
        let mut statement = tx.prepare("UPDATE turn SET state='stopped',ended_at=?2 WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait') RETURNING id")?;
        for id in statement.query_map(params![run, now()], |row| row.get(0))? {
            keys.push(Key::Turn(id?));
        }
        cancel_run_attempts(tx, run, &now())?;
        let mut statement = tx.prepare("UPDATE shell_execution SET state='stopped',output_complete=0,ended_at=?2,end_reason='run_stopped_before_spawn_or_collection' WHERE owner_run_id=?1 AND state IN ('starting','running','stopping') RETURNING id")?;
        for id in statement.query_map(params![run, now()], |row| row.get(0))? {
            keys.extend(Key::shell_terminal(id?));
        }
        if tx.execute("UPDATE run SET state='stopped',ended_at=?2,stop_reason=COALESCE(stop_reason,?3) WHERE id=?1 AND state IN ('live','stopping')",params![run,now(),if stopping_for_shutdown {Some("app_shutdown")} else {None}])? != 0 {
            keys.extend([Key::Run(run.clone()), Key::ThreadSettled(thread.clone())]);
        }
        if tx.execute("UPDATE receipt SET state='held',hold_reason='stopped',updated_at=?2 WHERE run_id=?1 AND state='accepted'",params![run,now()])? != 0 {
            keys.push(Key::CallerInput(thread.clone()));
        }
    }
    if let Some(thread) = repair_thread {
        super::message_delivery::repair_terminal_thread_tx(tx, thread, &now())?;
    } else {
        super::message_delivery::repair_terminal_tx(tx, &now())?;
    }
    Ok(())
}

/// Startup has saved the interrupted context; no previous work is replayed.
pub(crate) fn interrupt_previous_work(
    tx: &rusqlite::Transaction<'_>,
    now: &str,
    preserve_usage_waits: bool,
) -> Result<RepairReport, StorageError> {
    let preserved = super::usage_limit_retry::preserved_runs(tx, preserve_usage_waits)?;
    tx.execute(
        "DELETE FROM app_meta WHERE key GLOB 'compact.automatic_active.*'",
        [],
    )?;
    tx.execute("UPDATE turn SET state='stopped',ended_at=?1,recovery_reason_json=?2 WHERE state IN ('preparing','running','waiting','recovery_wait') AND run_id NOT IN (SELECT value FROM json_each(?3))",rusqlite::params![now,r#"{"reason":"previous_instance_interrupted","result_uncertain":true}"#,preserved])?;
    #[cfg(unix)]
    let settle = "UPDATE shell_execution SET state='failed',ended_at=?1,end_reason='previous_instance_interrupted_os_state_unverified',output_complete=0,shell_json=json_set(shell_json,'$.recovery',json('{\"os_state\":\"unverified\",\"result_uncertain\":true}')) WHERE state IN ('starting','running','stopping') RETURNING id,schedule_id";
    #[cfg(not(unix))]
    let settle = "UPDATE shell_execution SET state='stopped',ended_at=?1,end_reason='previous_instance_interrupted',output_complete=0 WHERE state IN ('starting','running','stopping') RETURNING id,schedule_id";
    let scheduled = tx
        .prepare(settle)?
        .query_map([now], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .filter_map(|row| match row {
            Ok((id, Some(_))) => Some(Ok(id)),
            Ok((_, None)) => None,
            Err(err) => Some(Err(err)),
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in scheduled {
        super::schedule::retire_oneshot(tx, &id)?;
    }
    tx.execute(
        "UPDATE schedule SET next_at=unixepoch(?1) WHERE kind='service' AND enabled=1",
        [now],
    )?;
    let report = repair_interrupted_tool_groups(tx, now)?;
    tx.execute("UPDATE run SET state='stopped',ended_at=?1,stop_reason=COALESCE(stop_reason,'previous_instance_interrupted') WHERE state IN ('live','stopping') AND id NOT IN (SELECT value FROM json_each(?2))",params![now,preserved])?;
    interrupt_previous_attempts(tx, now)?;
    tx.execute("UPDATE attachment_delivery SET status='held' WHERE status='accepted' AND (target_run_id IS NOT NULL OR json_extract(origin_json,'$.kind')='shell')",[])?;
    tx.execute("UPDATE receipt SET state='held',hold_reason='previous_instance_interrupted',updated_at=?1 WHERE state='accepted'",[&now])?;
    super::revisit::hand_off_tx(tx, None, "previous_instance_interrupted", None, now)?;
    super::message_delivery::repair_terminal_tx(tx, now)?;
    Ok(report)
}

/// Final fallback after all Control workers and accepted requests have drained.
/// Individual run stops above have already handled attempts/shells/receipts.
pub(super) fn finish_drained_shutdown(
    tx: &rusqlite::Transaction<'_>,
    preserve_usage_waits: bool,
) -> Result<(), StorageError> {
    let preserved = super::usage_limit_retry::preserved_runs(tx, preserve_usage_waits)?;
    tx.execute("UPDATE attachment_delivery SET status='held' WHERE status='accepted' AND (target_run_id IS NOT NULL OR json_extract(origin_json,'$.kind')='shell')",[])?;
    tx.execute("UPDATE run SET state='stopped',ended_at=?1,stop_reason=COALESCE(stop_reason,'app_shutdown') WHERE state IN ('live','stopping') AND id NOT IN (SELECT value FROM json_each(?2))",params![now(),preserved])?;
    tx.execute("UPDATE turn SET state='stopped',ended_at=?1 WHERE state IN ('preparing','running','waiting','recovery_wait') AND run_id NOT IN (SELECT value FROM json_each(?2))",params![now(),preserved])?;
    super::message_delivery::repair_terminal_tx(tx, &now())?;
    Ok(())
}

pub(super) fn complete_tool_call(
    db: &Connection,
    group: &str,
    call: &str,
) -> Result<usize, StorageError> {
    let changed=db.execute("UPDATE tool_call SET state='completed' WHERE group_id=?1 AND call_id=?2 AND state IN ('pending','running')",params![group,call])?;
    if changed == 0 {
        return Err(tool_transition_error(db, group, call)?.into());
    }
    Ok(changed)
}
pub(super) fn start_tool_call(
    db: &Connection,
    group: &str,
    call_id: &str,
    execution_id: &str,
) -> Result<usize, StorageError> {
    let changed=db.execute("UPDATE tool_call SET state='running',operation_id=?3 WHERE group_id=?1 AND call_id=?2 AND state='pending'",params![group,call_id,execution_id])?;
    if changed == 0 {
        return Err(tool_transition_error(db, group, call_id)?.into());
    }
    Ok(changed)
}
fn tool_transition_error(
    db: &Connection,
    group: &str,
    call: &str,
) -> Result<ApiError, StorageError> {
    let state: Option<String> = db
        .query_row(
            "SELECT state FROM tool_call WHERE group_id=?1 AND call_id=?2",
            params![group, call],
            |r| r.get(0),
        )
        .optional()?;
    let code = match state.as_deref() {
        None => ErrorCode::TargetUnavailable,
        Some("pending" | "running" | "completed") => ErrorCode::OperationConflict,
        _ => ErrorCode::InvalidState,
    };
    Ok(ApiError::new(
        code,
        "Tool call is not at its expected transition boundary",
    ))
}
pub(super) fn fail_shell_spawn(
    db: &Connection,
    execution: &str,
    at: &str,
) -> Result<usize, StorageError> {
    finish_shell_spawn(db, execution, at, false)
}
pub(super) fn cancel_shell_spawn(
    db: &Connection,
    execution: &str,
    at: &str,
) -> Result<usize, StorageError> {
    finish_shell_spawn(db, execution, at, true)
}
fn finish_shell_spawn(
    db: &Connection,
    execution: &str,
    at: &str,
    cancelled: bool,
) -> Result<usize, StorageError> {
    // A saved stop intent can still be stopping when OS spawn fails. Settle
    // that admitted execution, but never overwrite an already terminal result.
    let changed=db.execute("UPDATE shell_execution SET state=?3,end_reason=CASE WHEN end_reason='owner_shutdown' THEN end_reason ELSE ?4 END,output_complete=0,ended_at=?2 WHERE id=?1 AND state IN ('starting','stopping')",params![execution,at,if cancelled {"stopped"} else {"failed"},if cancelled {"stopped_before_spawn"} else {"spawn_failed"}])?;
    if changed == 0 {
        let state: Option<String> = db
            .query_row(
                "SELECT state FROM shell_execution WHERE id=?1",
                [execution],
                |r| r.get(0),
            )
            .optional()?;
        return match state.as_deref() {
            Some("exited" | "failed" | "stopped") => Ok(0),
            Some("starting" | "running" | "stopping") => Err(ApiError::new(
                ErrorCode::OperationConflict,
                "Shell spawn result is no longer current",
            )
            .into()),
            None => Err(ApiError::new(
                ErrorCode::TargetUnavailable,
                "Shell execution no longer exists",
            )
            .into()),
            _ => Err(ApiError::new(ErrorCode::InvalidState, "Unknown saved shell state").into()),
        };
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::request_path::{fail_attempt, incomplete_attempt};
    use serde_json::json;

    fn fixture(state: &str) -> Connection {
        crate::control::test_support::terminal_attempt_fixture("attempt", state)
    }
    fn saved(db: &Connection) -> (String, String, String) {
        db.query_row(
            "SELECT state,error_json,ended_at FROM model_attempt WHERE id='attempt'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    #[test]
    fn late_completion_cannot_rewrite_a_saved_answer_or_end_time() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch(
            "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');
             INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','live','now');
             INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','running','now');
             INSERT INTO node(id,turn_id,kind,created_at) VALUES('answer','u','assistant_response','now');",
        ).unwrap();
        let tx = db.transaction().unwrap();
        let (run, turn, answer) = ("r", "u", "answer");
        for run_state in ["completed", "stopped", "failed", "stopping"] {
            for turn_state in ["completed", "stopped", "failed"] {
                tx.execute(
                    "UPDATE run SET state=?1,ended_at='original-run'",
                    [run_state],
                )
                .unwrap();
                tx.execute(
                    "UPDATE turn SET state=?1,ended_at='original-turn',answer_node_id=?2",
                    params![turn_state, answer],
                )
                .unwrap();
                complete_idle_run(&tx, run).unwrap();
                complete_answer_turn(&tx, turn, answer).unwrap();
                assert_eq!(
                    tx.query_row("SELECT state,ended_at FROM run", [], |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?
                    )))
                    .unwrap(),
                    (run_state.into(), "original-run".into())
                );
                assert_eq!(
                    tx.query_row("SELECT state,ended_at FROM turn", [], |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?
                    )))
                    .unwrap(),
                    (turn_state.into(), "original-turn".into())
                );
            }
        }
        tx.execute("UPDATE run SET state='completed'", []).unwrap();
        tx.execute("UPDATE turn SET state='completed'", []).unwrap();
        assert_eq!(
            complete_answer_turn(&tx, turn, "different-answer")
                .unwrap_err()
                .into_api_error()
                .code,
            ErrorCode::OperationConflict
        );
        assert_eq!(
            tx.query_row("SELECT answer_node_id FROM turn", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            answer
        );
        assert_eq!(
            complete_idle_run(&tx, "missing")
                .unwrap_err()
                .into_api_error()
                .code,
            ErrorCode::TargetUnavailable
        );
        assert_eq!(
            complete_answer_turn(&tx, "missing", answer)
                .unwrap_err()
                .into_api_error()
                .code,
            ErrorCode::TargetUnavailable
        );
    }

    #[test]
    fn executor_preserves_provider_body_without_writing_compaction_facts() {
        for current in [None, Some("owned")] {
            let db = fixture("inflight");
            db.execute_batch(
                "CREATE TABLE automatic_compaction(attempt_id TEXT PRIMARY KEY,token TEXT)",
            )
            .unwrap();
            if let Some(token) = current {
                db.execute(
                    "INSERT INTO automatic_compaction VALUES('attempt',?1)",
                    [token],
                )
                .unwrap();
            }
            let forged = json!({"error":{"kind":"provider","message":"failed"},"automatic_trim_attempt":{"token":"forged","discard":{"reason":"profile_changed"}}});
            assert_eq!(
                fail_attempt(&db, "attempt", &forged, "now").unwrap(),
                AttemptTransition::Applied
            );
            let body: Value = serde_json::from_str(&saved(&db).1).unwrap();
            assert_eq!(body, forged);
            let token: Option<String> = db
                .query_row(
                    "SELECT token FROM automatic_compaction WHERE attempt_id='attempt'",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            assert_eq!(token.as_deref(), current);
        }
    }
    #[test]
    fn terminal_usage_is_normalized_atomically() {
        let usage = json!({"input_tokens":123,"input_tokens_details":{"cached_tokens":0,"extra":"x".repeat(128*1024)},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":-1},"total_tokens":127,"attribution":"x".repeat(128*1024)});
        let canonical = json!({"input_tokens":123,"input_tokens_details":{"cached_tokens":0},"output_tokens":4,"total_tokens":127});
        for operation in [fail_attempt, incomplete_attempt] {
            let mut db = fixture("inflight");
            let body = json!({"id":"response","usage":usage,"raw_response":{"usage":usage,"output":[{"usage":"user content"}]},"error":{"kind":"fixture"}});
            // Transaction rollback must discard both terminal state and usage.
            {
                let tx = db.transaction().unwrap();
                assert_eq!(
                    operation(&tx, "attempt", &body, "new").unwrap(),
                    AttemptTransition::Applied
                );
            }
            let read = |db: &Connection| {
                db.query_row(
                    "SELECT state,usage_json,error_json FROM model_attempt WHERE id='attempt'",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .unwrap()
            };
            assert_eq!(read(&db).0, "inflight");
            assert_eq!(read(&db).1.as_deref(), Some("{}"));
            assert_eq!(
                operation(&db, "attempt", &body, "new").unwrap(),
                AttemptTransition::Applied
            );
            let saved = read(&db);
            assert_eq!(
                serde_json::from_str::<Value>(saved.1.as_deref().unwrap()).unwrap(),
                canonical
            );
            if saved.0 != "completed" {
                let error: Value = serde_json::from_str(&saved.2).unwrap();
                assert_eq!(error["raw_response"]["usage"], canonical);
                assert_eq!(
                    error["raw_response"]["output"],
                    body["raw_response"]["output"]
                );
                assert_eq!(error["error"], body["error"]);
            }
        }
    }
    #[test]
    fn adoption_transition_respects_predecessor_and_generation() {
        for (state, expected) in [
            ("completed", AttemptTransition::Applied),
            ("failed", AttemptTransition::Obsolete),
            ("incomplete", AttemptTransition::Obsolete),
            ("cancelled", AttemptTransition::Obsolete),
        ] {
            let db = fixture(state);
            let before = saved(&db);
            assert_eq!(adopt_attempt(&db, "attempt").unwrap(), expected);
            if expected != AttemptTransition::Applied {
                assert_eq!(saved(&db), before);
            }
        }
        let db = fixture("completed");
        db.execute("UPDATE turn SET generation=generation+1", [])
            .unwrap();
        let before = saved(&db);
        assert_eq!(
            adopt_attempt(&db, "attempt").unwrap(),
            AttemptTransition::Obsolete
        );
        assert_eq!(saved(&db), before);
    }
    #[test]
    fn spawn_failure_settles_stopping_but_never_rewrites_terminal_shell_facts() {
        for (state, changed) in [
            ("starting", 1),
            ("stopping", 1),
            ("exited", 0),
            ("failed", 0),
            ("stopped", 0),
        ] {
            let db = fixture("inflight");
            db.execute_batch("CREATE TABLE shell_execution(id TEXT PRIMARY KEY,state TEXT,end_reason TEXT,output_complete INTEGER,ended_at TEXT);").unwrap();
            db.execute(
                "INSERT INTO shell_execution VALUES('shell',?1,'original',1,'original')",
                [state],
            )
            .unwrap();
            assert_eq!(fail_shell_spawn(&db, "shell", "late").unwrap(), changed);
            let row = db
                .query_row(
                    "SELECT state,end_reason,output_complete,ended_at FROM shell_execution",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    },
                )
                .unwrap();
            if changed == 1 {
                assert_eq!(
                    row,
                    ("failed".into(), "spawn_failed".into(), 0, "late".into())
                );
            } else {
                assert_eq!(row, (state.into(), "original".into(), 1, "original".into()));
            }
        }
    }
    #[test]
    fn tool_transition_missing_unknown_and_duplicate_are_not_storage_failures() {
        for (state, code) in [
            ("completed", ErrorCode::OperationConflict),
            ("unknown", ErrorCode::InvalidState),
        ] {
            let db = fixture("inflight");
            db.execute_batch(
                "CREATE TABLE tool_call(group_id TEXT,call_id TEXT,state TEXT,operation_id TEXT);",
            )
            .unwrap();
            db.execute(
                "INSERT INTO tool_call VALUES('group','call',?1,'original')",
                [state],
            )
            .unwrap();
            assert_eq!(
                start_tool_call(&db, "group", "call", "new")
                    .unwrap_err()
                    .into_api_error()
                    .code,
                code
            );
            assert_eq!(
                complete_tool_call(&db, "group", "call")
                    .unwrap_err()
                    .into_api_error()
                    .code,
                code
            );
            assert_eq!(
                start_tool_call(&db, "group", "missing", "new")
                    .unwrap_err()
                    .into_api_error()
                    .code,
                ErrorCode::TargetUnavailable
            );
            assert_eq!(
                db.query_row("SELECT state,operation_id FROM tool_call", [], |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?
                )))
                .unwrap(),
                (state.into(), "original".into())
            );
        }
    }
}

#[cfg(test)]
mod exit_reason_tests {
    use super::*;
    #[test]
    fn startup_removes_only_automatic_compaction_witnesses() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO app_meta(key,value) VALUES('compact.automatic_active.thread','{}'),('compact.automaticXactive.thread','unrelated');").unwrap();
        let tx = db.transaction().unwrap();
        interrupt_previous_work(&tx, "now", false).unwrap();
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM app_meta WHERE key='compact.automatic_active.thread'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            tx.query_row(
                "SELECT value FROM app_meta WHERE key='compact.automaticXactive.thread'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "unrelated"
        );
    }
    #[test]
    fn startup_and_both_shutdown_paths_preserve_explicit_stop() {
        for boundary in ["startup", "drained", "fallback"] {
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/','p','m','old'); INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('u','/','/','p','m','old'); INSERT INTO run(id,thread_id,state,created_at,stop_reason) VALUES('explicit','t','stopping','old','explicit_stop'); INSERT INTO run(id,thread_id,state,created_at) VALUES('unclassified','u','live','old');").unwrap();
            let tx = db.transaction().unwrap();
            match boundary {
                "startup" => {
                    interrupt_previous_work(&tx, "now", false).unwrap();
                }
                "drained" => stop_drained_runs(
                    &tx,
                    &[
                        ("explicit".into(), "t".into()),
                        ("unclassified".into(), "u".into()),
                    ],
                    true,
                    &mut Vec::new(),
                )
                .unwrap(),
                _ => finish_drained_shutdown(&tx, false).unwrap(),
            }
            assert_eq!(
                tx.query_row("SELECT stop_reason FROM run WHERE id='explicit'", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "explicit_stop",
                "{boundary}"
            );
            assert_eq!(
                tx.query_row(
                    "SELECT stop_reason FROM run WHERE id='unclassified'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                if boundary == "startup" {
                    "previous_instance_interrupted"
                } else {
                    "app_shutdown"
                },
                "{boundary}"
            );
            tx.commit().unwrap();
        }
    }
}

#[cfg(test)]
mod cache_diagnostic_tests {
    use super::*;
    #[tokio::test]
    async fn diagnostics_are_gated_at_receipt_and_adoption_preserves_them() {
        for operation in [
            None,
            Some(
                crate::model::request_path::fail_attempt
                    as fn(
                        &Connection,
                        &str,
                        &Value,
                        &str,
                    ) -> Result<AttemptTransition, StorageError>,
            ),
            Some(crate::model::request_path::incomplete_attempt),
        ] {
            for enabled in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let worker = crate::storage::DbWorker::open(root.path().join("fixture.db"))
                    .await
                    .unwrap();
                let raw = json!({"id":"native-id","status":"completed","output":[],"prompt_cache_diagnostics":{"type":"cache_miss","reason":"reported","cache_missed_tokens":0,"unknown":"x".repeat(8192)}});
                for (generation, expected_live) in [(1, false), (0, true)] {
                    let failure_raw = raw.clone();
                    worker.call(move |db| {
                    if !expected_live {
                db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now');").unwrap();
                let manifest=json!({"prompt_cache":{"diagnostics_enabled":enabled,"comparison_response_id":null},"prompt_cache_diagnostics":null}).to_string();
                db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'p','m',0,'inflight',?1,'now')",[&manifest]).unwrap();
                    }
                    db.execute("UPDATE turn SET generation=?1", [generation])?;
                    if let Some(write) = operation.filter(|_| expected_live) {
                        let body = json!({"raw_response":failure_raw});
                        assert_eq!(write(db,"a",&body,"done")?, if expected_live { AttemptTransition::Applied } else { AttemptTransition::Obsolete });
                    }
                    Ok(())
                    }).await.unwrap();
                    if operation.is_none() {
                        let mut response =
                            crate::model::ModelResponse::completed(raw.clone(), false).unwrap();
                        response.raw["prompt_cache_diagnostics"] =
                            raw["prompt_cache_diagnostics"].clone();
                        crate::model::request_path::persist_received_attempt(
                            &worker,
                            "a",
                            crate::model::request_path::AttemptKind::Conversation,
                            false,
                            &Ok(response),
                        )
                        .await
                        .unwrap();
                        worker
                            .call(move |db| {
                                assert_eq!(
                                    adopt_attempt(db, "a")?,
                                    if expected_live {
                                        AttemptTransition::Applied
                                    } else {
                                        AttemptTransition::Obsolete
                                    }
                                );
                                Ok(())
                            })
                            .await
                            .unwrap();
                    }
                    let saved = worker
                        .call(|db| {
                            Ok(db.query_row(
                                "SELECT input_manifest_json FROM model_attempt",
                                [],
                                |r| r.get::<_, String>(0),
                            )?)
                        })
                        .await
                        .unwrap();
                    let saved: Value = serde_json::from_str(&saved).unwrap();
                    assert_eq!(
                        saved["prompt_cache_diagnostics"],
                        if enabled && (expected_live || operation.is_none()) {
                            json!({"type":"cache_miss","reason":"reported","cache_missed_tokens":0})
                        } else {
                            Value::Null
                        }
                    );
                }
                worker.shutdown().await.unwrap();
            }
        }
    }
}

// Audit-only received responses are retained through their live Turn, outside
// thread_path/model replay. Share the same atomic insert/root revision boundary
// for explicit recovery and automatic partial-response retries.
pub(crate) fn save_received_failure(
    db: &rusqlite::Connection,
    turn: &str,
    body: &Value,
) -> Result<(), crate::storage::StorageError> {
    let body = body.to_string();
    db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes)VALUES(?1,?2,'incomplete_response',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),turn,now(),body,body.len() as i64])?;
    if db.execute(
        "UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision'",
        [],
    )? != 1
    {
        return Err(crate::storage::StorageError::Schema(
            "root revision metadata missing".into(),
        ));
    }
    Ok(())
}
