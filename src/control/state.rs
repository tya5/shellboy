//! Persistent state changes, executed on the existing caller DB job/transaction.
//! Terminal updates preserve settled audit data and classify zero-row outcomes.
use crate::{
    protocol::{ApiError, AttemptState, ErrorCode, RunState, TurnState},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(super) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Create the Turn and its existing context observation in the caller's
/// transaction. Admission predicates and wake/recovery policy stay with callers.
pub(super) fn create_turn_tx(
    tx: &rusqlite::Transaction<'_>,
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
) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE turn SET state='preparing',generation=generation+1,recovery_reason_json=NULL
         WHERE id=?1 AND thread_id=?2 AND run_id=?3 AND generation=?4 AND state='recovery_wait'
         AND EXISTS(SELECT 1 FROM run r WHERE r.id=turn.run_id AND r.state='live')",
        params![turn, thread, run, generation],
    )?;
    if changed != 1 {
        return Ok(false);
    }
    tx.execute(
        "UPDATE receipt SET state='accepted',hold_reason=NULL,updated_at=?4
        WHERE turn_id=?1 AND target_thread_id=?2 AND run_id=?3
        AND state='held' AND hold_reason='recovery_wait'",
        params![turn, thread, run, at],
    )?;
    Ok(true)
}

/// A DB transition result, not a second in-memory attempt state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttemptTransition {
    Applied,
    AlreadyApplied,
    Obsolete,
}

const ACTIVE_ATTEMPTS: &[AttemptState] = &[AttemptState::Preparing, AttemptState::Inflight];
const REQUEST_TURNS: &[TurnState] = &[TurnState::Preparing, TurnState::Running, TurnState::Waiting];

pub(super) fn fail_attempt(
    db: &Connection,
    attempt: &str,
    error: &Value,
    at: &str,
) -> Result<AttemptTransition, StorageError> {
    let changed = db.execute(
        "UPDATE model_attempt SET state='failed',error_json=?2,ended_at=?3
         WHERE id=?1 AND state IN ('preparing','inflight')
         AND EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id
                    WHERE u.id=model_attempt.turn_id AND u.generation=model_attempt.generation
                    AND u.state IN ('preparing','running','waiting') AND r.state='live')",
        params![attempt, error.to_string(), at],
    )?;
    attempt_outcome(db, attempt, AttemptState::Failed, changed)
}
pub(super) fn complete_attempt(
    db: &Connection,
    attempt: &str,
    response: &Value,
    at: &str,
) -> Result<AttemptTransition, StorageError> {
    let changed = db.execute(
        "UPDATE model_attempt SET state='completed',response_id=?2,usage_json=?3,ended_at=?4
         WHERE id=?1 AND state='inflight'
         AND EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id
                    WHERE u.id=model_attempt.turn_id AND u.generation=model_attempt.generation
                    AND u.state='running' AND r.state='live')",
        params![
            attempt,
            response.get("id").and_then(Value::as_str),
            response.get("usage").map(Value::to_string),
            at
        ],
    )?;
    attempt_outcome(db, attempt, AttemptState::Completed, changed)
}
fn failed_context_limit(body: Option<&str>) -> bool {
    #[derive(serde::Deserialize)]
    struct SavedFailure {
        error: crate::model::ModelError,
    }
    body.and_then(|body| serde_json::from_str::<SavedFailure>(body).ok())
        .is_some_and(|saved| saved.error.is_context_limit())
}

pub(super) fn incomplete_attempt(
    db: &Connection,
    attempt: &str,
    error: &Value,
    at: &str,
) -> Result<AttemptTransition, StorageError> {
    // Only a saved, typed context-limit failure may be reclassified after
    // reduction. CAS the exact saved cause so caller data cannot replace it.
    let saved_failure: Option<Option<String>> = db
        .query_row(
            "SELECT error_json FROM model_attempt WHERE id=?1 AND state='failed'",
            [attempt],
            |row| row.get(0),
        )
        .optional()?;
    let permitted_failure = saved_failure
        .flatten()
        .filter(|body| failed_context_limit(Some(body)));
    let changed = db.execute(
        "UPDATE model_attempt SET state='incomplete',error_json=?2,ended_at=?3
         WHERE id=?1 AND (state IN ('preparing','inflight') OR (state='failed' AND error_json=?4))
         AND EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id
                    WHERE u.id=model_attempt.turn_id AND u.generation=model_attempt.generation
                    AND u.state IN ('preparing','running','waiting','recovery_wait') AND r.state='live')",
        params![attempt, error.to_string(), at, permitted_failure],
    )?;
    attempt_outcome(db, attempt, AttemptState::Incomplete, changed)
}
fn attempt_outcome(
    db: &Connection,
    attempt: &str,
    target: AttemptState,
    changed: usize,
) -> Result<AttemptTransition, StorageError> {
    if changed == 1 {
        return Ok(AttemptTransition::Applied);
    }
    let row = db
        .query_row(
            "SELECT a.state,a.generation,u.generation,u.state,r.state,a.error_json
         FROM model_attempt a JOIN turn u ON u.id=a.turn_id JOIN run r ON r.id=u.run_id
         WHERE a.id=?1",
            [attempt],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::TargetUnavailable,
                "Model attempt is no longer available",
            )
        })?;
    let (state, generation, turn_generation, turn, run, saved_error) = row;
    let state: AttemptState = serde_json::from_value(Value::String(state))
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved model attempt state"))?;
    let turn: TurnState = serde_json::from_value(Value::String(turn))
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved Turn state"))?;
    let run: RunState = serde_json::from_value(Value::String(run))
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Unknown saved Run state"))?;
    if generation != turn_generation || run != RunState::Live {
        return Ok(AttemptTransition::Obsolete);
    }
    if state == target {
        return Ok(AttemptTransition::AlreadyApplied);
    }
    let active_turn = match target {
        AttemptState::Completed => turn == TurnState::Running,
        AttemptState::Incomplete => {
            REQUEST_TURNS.contains(&turn) || turn == TurnState::RecoveryWait
        }
        _ => REQUEST_TURNS.contains(&turn),
    };
    if !active_turn
        || matches!(
            state,
            AttemptState::Completed | AttemptState::Incomplete | AttemptState::Cancelled
        )
        || (state == AttemptState::Failed && target != AttemptState::Incomplete)
    {
        return Ok(AttemptTransition::Obsolete);
    }
    let predecessor_permits = match target {
        AttemptState::Completed => state == AttemptState::Inflight,
        AttemptState::Incomplete => {
            ACTIVE_ATTEMPTS.contains(&state)
                || (state == AttemptState::Failed && failed_context_limit(saved_error.as_deref()))
        }
        _ => ACTIVE_ATTEMPTS.contains(&state),
    };
    let (code, message) = if predecessor_permits {
        (
            ErrorCode::OperationConflict,
            "Model attempt was not changed at its expected boundary",
        )
    } else {
        (
            ErrorCode::InvalidState,
            "Model attempt predecessor does not permit this transition",
        )
    };
    Err(ApiError::new(code, message).into())
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
) -> Result<(), crate::storage::StorageError> {
    let changed=tx.execute("UPDATE turn SET state='recovery_wait',recovery_reason_json=?2 WHERE id=?1 AND generation=?3 AND state IN ('preparing','running','waiting')",params![turn,serde_json::to_string(error).unwrap_or_default(),generation])?;
    if changed > 0 {
        tx.execute(
            "UPDATE attachment_delivery SET status='held' WHERE thread_id=?1 AND status='accepted'",
            [thread],
        )?;
        owner_event(
            tx,
            run,
            &format!("child-recovery-{turn}-{generation}"),
            "child_recovery_wait",
            json!({"thread_id":thread,"run_id":run,"turn_id":turn,"generation":generation,"state":"recovery_wait","error":error}),
        )?;
    }
    Ok(())
}

pub(super) fn owner_event(
    tx: &rusqlite::Transaction<'_>,
    child_run: &str,
    event_id: &str,
    kind: &str,
    payload: Value,
) -> Result<(), crate::storage::StorageError> {
    let owner:Option<(String,String)>=tx.query_row("SELECT parent.id,parent.thread_id FROM run child JOIN run parent ON parent.id=child.owner_run_id WHERE child.id=?1",[child_run],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    if let Some((run, thread)) = owner {
        tx.execute("INSERT OR IGNORE INTO event(id,kind,source_json,target_thread_id,target_run_id,payload_ref_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![event_id,kind,json!({"run_id":child_run}).to_string(),thread,run,payload.to_string(),now()])?;
        tx.execute(
            "INSERT OR IGNORE INTO delivery(event_id,recipient_id,state) VALUES(?1,?2,'pending')",
            params![event_id, run],
        )?;
    }
    Ok(())
}

/// next() has checked that no Turn, pending input, or owned live work remains
/// in this same transaction; preserve its normal completion notification.
pub(super) fn complete_idle_run(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
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
    owner_event(
        tx,
        run,
        &format!("child-run-ended-{run}"),
        "child_run_ended",
        json!({"thread_id":thread,"run_id":run,"state":"completed"}),
    )?;
    Ok(())
}

/// A normal completed response with no calls is saved in this transaction.
/// New accepted input keeps this Turn open for the following request.
pub(super) fn complete_answer_turn(
    tx: &rusqlite::Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    node: &str,
) -> Result<(), StorageError> {
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
                    Ok(())
                } else {
                    Err(ApiError::new(
                        ErrorCode::OperationConflict,
                        "Turn already has a different answer",
                    )
                    .into())
                };
            }
            if matches!(state, TurnState::Stopped | TurnState::Failed) || owner != RunState::Live {
                return Ok(()); // Obsolete completion cannot revive a settled Turn.
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
        owner_event(
            tx,
            run,
            &format!("child-turn-completed-{}", turn),
            "child_turn_completed",
            json!({"thread_id":thread,"run_id":run,"turn_id":turn,"state":"completed","answer_node_id":node}),
        )?;
    }
    Ok(())
}

/// Ownership has already been frozen, and every live handle has drained.
pub(super) fn stop_drained_runs(
    tx: &rusqlite::Transaction<'_>,
    owned: &[(String, String)],
    stopping_for_shutdown: bool,
) -> Result<(), StorageError> {
    for (run, _) in owned {
        tx.execute("UPDATE turn SET state='stopped',ended_at=?2 WHERE run_id=?1 AND state IN ('preparing','running','waiting','recovery_wait')",params![run,now()])?;
        cancel_run_attempts(tx, run, &now())?;
        tx.execute("UPDATE shell_execution SET state='stopped',output_complete=0,ended_at=?2,end_reason='run_stopped_before_spawn_or_collection' WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')",params![run,now()])?;
        let changed=tx.execute("UPDATE run SET state='stopped',ended_at=?2,stop_reason=COALESCE(stop_reason,?3) WHERE id=?1 AND state IN ('live','stopping')",params![run,now(),if stopping_for_shutdown {Some("app_shutdown")} else {None}])?;
        if changed > 0 {
            owner_event(
                tx,
                run,
                &format!("child-run-ended-{run}"),
                "child_run_ended",
                json!({"run_id":run,"state":"stopped","answer_node_id":null,"incomplete":true}),
            )?;
        }
        tx.execute("UPDATE receipt SET state='held',hold_reason='stopped',updated_at=?2 WHERE run_id=?1 AND state='accepted'",params![run,now()])?;
    }
    Ok(())
}

/// Startup has saved the interrupted context; no previous work is replayed.
pub(crate) fn interrupt_previous_work(
    tx: &rusqlite::Transaction<'_>,
    now: &str,
) -> Result<(), StorageError> {
    tx.execute("UPDATE turn SET state='stopped',ended_at=?1,recovery_reason_json=?2 WHERE state IN ('preparing','running','waiting','recovery_wait')",rusqlite::params![now,r#"{"reason":"previous_instance_interrupted","result_uncertain":true}"#])?;
    tx.execute("UPDATE shell_execution SET state='stopped',ended_at=?1,end_reason='previous_instance_interrupted',output_complete=0 WHERE state IN ('starting','running','stopping')",[&now])?;
    tx.execute("UPDATE run SET state='stopped',ended_at=?1,stop_reason='previous_instance_interrupted' WHERE state IN ('live','stopping')",[&now])?;
    interrupt_previous_attempts(tx, now)?;
    tx.execute("UPDATE attachment_delivery SET status='held' WHERE status='accepted' AND (target_run_id IS NOT NULL OR json_extract(origin_json,'$.kind')='shell')",[])?;
    tx.execute("UPDATE receipt SET state='held',hold_reason='previous_instance_interrupted',updated_at=?1 WHERE state IN ('accepted','held')",[&now])?;
    Ok(())
}

/// Final fallback after all Control workers and accepted requests have drained.
/// Individual run stops above have already handled attempts/shells/receipts.
pub(super) fn finish_drained_shutdown(tx: &rusqlite::Transaction<'_>) -> Result<(), StorageError> {
    tx.execute("UPDATE attachment_delivery SET status='held' WHERE status='accepted' AND (target_run_id IS NOT NULL OR json_extract(origin_json,'$.kind')='shell')",[])?;
    tx.execute("UPDATE run SET state='stopped',ended_at=?1,stop_reason=COALESCE(stop_reason,'app_shutdown') WHERE state IN ('live','stopping')",[chrono::Utc::now().to_rfc3339()])?;
    tx.execute("UPDATE turn SET state='stopped',ended_at=?1 WHERE state IN ('preparing','running','waiting','recovery_wait')",[chrono::Utc::now().to_rfc3339()])?;
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
    // A saved stop intent can still be stopping when OS spawn fails. Settle
    // that admitted execution, but never overwrite an already terminal result.
    let changed=db.execute("UPDATE shell_execution SET state='failed',end_reason='spawn_failed',output_complete=0,ended_at=?2 WHERE id=?1 AND state IN ('starting','stopping')",params![execution,at])?;
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
    use serde_json::json;

    fn fixture(state: &str) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE run(id TEXT PRIMARY KEY,state TEXT); CREATE TABLE turn(id TEXT PRIMARY KEY,run_id TEXT,state TEXT,generation INTEGER); CREATE TABLE model_attempt(id TEXT PRIMARY KEY,turn_id TEXT,state TEXT,generation INTEGER,error_json TEXT,response_id TEXT,usage_json TEXT,ended_at TEXT); INSERT INTO run VALUES('run','live'); INSERT INTO turn VALUES('turn','run','running',1);").unwrap();
        db.execute("INSERT INTO model_attempt VALUES('attempt','turn',?1,1,'{\"original\":true}','original','{}','original')", [state]).unwrap();
        db
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
    fn late_failures_never_reclassify_completed_or_cancelled_attempts() {
        for state in ["completed", "cancelled"] {
            let db = fixture(state);
            let original = saved(&db);
            fail_attempt(&db, "attempt", &json!({"late":true}), "late").unwrap();
            assert_eq!(saved(&db), original);
            incomplete_attempt(&db, "attempt", &json!({"late":true}), "late").unwrap();
            assert_eq!(saved(&db), original);
        }
    }
    #[test]
    fn attempt_results_from_an_old_generation_cannot_replace_current_audit_fields() {
        for state in ["inflight", "failed"] {
            for operation in [fail_attempt, incomplete_attempt, complete_attempt] {
                let db = fixture(state);
                db.execute("UPDATE turn SET generation=2", []).unwrap();
                let original = saved(&db);
                assert_eq!(
                    operation(&db, "attempt", &json!({"id":"late"}), "late").unwrap(),
                    AttemptTransition::Obsolete
                );
                assert_eq!(saved(&db), original);
            }
        }
    }
    #[test]
    fn attempt_terminal_boundaries_cover_every_saved_predecessor() {
        use AttemptTransition::{AlreadyApplied, Applied, Obsolete};
        // Columns: failure, completion, incomplete recovery. None is a
        // genuinely invalid predecessor, distinct from a settled late result.
        let cases = [
            ("preparing", Some(Applied), None, Some(Applied)),
            ("inflight", Some(Applied), Some(Applied), Some(Applied)),
            ("failed", Some(AlreadyApplied), Some(Obsolete), None),
            (
                "completed",
                Some(Obsolete),
                Some(AlreadyApplied),
                Some(Obsolete),
            ),
            (
                "incomplete",
                Some(Obsolete),
                Some(Obsolete),
                Some(AlreadyApplied),
            ),
            ("cancelled", Some(Obsolete), Some(Obsolete), Some(Obsolete)),
        ];
        for (state, fail, complete, incomplete) in cases {
            for ((operation, target), expected) in [
                (
                    fail_attempt as fn(&Connection, &str, &Value, &str) -> _,
                    "failed",
                ),
                (complete_attempt, "completed"),
                (incomplete_attempt, "incomplete"),
            ]
            .into_iter()
            .zip([fail, complete, incomplete])
            {
                let db = fixture(state);
                let original = saved(&db);
                let result = operation(
                    &db,
                    "attempt",
                    &json!({"id":"new","usage":{"tokens":1}}),
                    "new",
                );
                match expected {
                    Some(outcome) => {
                        assert_eq!(result.unwrap(), outcome, "{state} -> {target}");
                        if outcome == Applied {
                            let actual = saved(&db);
                            assert_eq!(actual.0, target);
                            assert_eq!(actual.2, "new");
                        } else {
                            assert_eq!(saved(&db), original);
                        }
                    }
                    None => assert_eq!(
                        result.unwrap_err().into_api_error().code,
                        ErrorCode::InvalidState
                    ),
                }
            }
        }
    }
    #[test]
    fn failed_raw_survives_reduction_and_incomplete_reclassification_rolls_back_atomically() {
        let mut db = fixture("inflight");
        let raw = json!({"error":crate::model::ModelError::provider(Some(400), &json!({"error":{"code":"context_length_exceeded","message":"context limit"}})),"raw_response":"context-limit","partial_text":"audit"});
        assert_eq!(
            fail_attempt(&db, "attempt", &raw, "failed").unwrap(),
            AttemptTransition::Applied
        );
        let failed = saved(&db);
        assert_eq!(failed.1, raw.to_string());
        db.execute("UPDATE turn SET state='recovery_wait'", [])
            .unwrap();
        {
            let tx = db.transaction().unwrap();
            assert_eq!(
                incomplete_attempt(&tx, "attempt", &raw, "incomplete").unwrap(),
                AttemptTransition::Applied
            );
            assert_eq!(saved(&tx).0, "incomplete");
            tx.rollback().unwrap();
        }
        assert_eq!(
            saved(&db),
            failed,
            "a failed recovery transaction keeps the previously committed raw"
        );
        assert_eq!(
            incomplete_attempt(&db, "attempt", &raw, "incomplete").unwrap(),
            AttemptTransition::Applied
        );
        assert_eq!(
            saved(&db),
            ("incomplete".into(), raw.to_string(), "incomplete".into())
        );
    }
    #[test]
    fn stopped_ownership_and_terminal_turns_make_late_attempt_results_obsolete() {
        for update in [
            "UPDATE run SET state='stopping'",
            "UPDATE run SET state='stopped'",
            "UPDATE turn SET state='stopped'",
            "UPDATE turn SET state='completed'",
        ] {
            for operation in [fail_attempt, incomplete_attempt, complete_attempt] {
                let db = fixture("inflight");
                db.execute(update, []).unwrap();
                let original = saved(&db);
                assert_eq!(
                    operation(&db, "attempt", &json!({}), "late").unwrap(),
                    AttemptTransition::Obsolete
                );
                assert_eq!(saved(&db), original);
            }
        }
    }
    #[test]
    fn failed_incomplete_reclassification_requires_the_saved_typed_context_limit() {
        for code in [
            "context_length_exceeded",
            "max_context_length_exceeded",
            "context_window_exceeded",
        ] {
            let db = fixture("failed");
            let raw = json!({"error":{"kind":"provider","message":"context limit","provider_code":code,"status":400},"raw_response":"paid received text"});
            db.execute("UPDATE model_attempt SET error_json=?1", [raw.to_string()])
                .unwrap();
            assert_eq!(
                incomplete_attempt(&db, "attempt", &raw, "new").unwrap(),
                AttemptTransition::Applied
            );
            assert_eq!(
                saved(&db),
                ("incomplete".into(), raw.to_string(), "new".into())
            );
        }
        for body in [
            "not JSON".to_owned(),
            json!({"error":{"provider_code":"context_length_exceeded"}}).to_string(),
            json!({"error":{"kind":"provider","message":"auth","provider_code":"invalid_api_key","status":401}}).to_string(),
            json!({"error":{"kind":"transport","message":"network unavailable"}}).to_string(),
            json!({"error":{"kind":"provider","message":"context_length_exceeded"}}).to_string(),
        ] {
            let db = fixture("failed");
            db.execute("UPDATE model_attempt SET error_json=?1",[body]).unwrap();
            let before = saved(&db);
            let forged = json!({"error":{"kind":"provider","message":"new claimed cause","provider_code":"context_length_exceeded"}});
            assert_eq!(incomplete_attempt(&db,"attempt",&forged,"new").unwrap_err().into_api_error().code,ErrorCode::InvalidState);
            assert_eq!(saved(&db),before,"incoming error cannot reclassify an unrelated saved failure");
        }
    }

    #[test]
    fn missing_unknown_and_real_database_failures_remain_distinct() {
        let db = fixture("unknown");
        let error = fail_attempt(&db, "missing", &json!({}), "now")
            .unwrap_err()
            .into_api_error();
        assert_eq!(error.code, ErrorCode::TargetUnavailable);
        let error = fail_attempt(&db, "attempt", &json!({}), "now")
            .unwrap_err()
            .into_api_error();
        assert_eq!(error.code, ErrorCode::InvalidState);
        db.execute("DROP TABLE model_attempt", []).unwrap();
        let error = fail_attempt(&db, "attempt", &json!({}), "now")
            .unwrap_err()
            .into_api_error();
        assert_eq!(error.code, ErrorCode::SaveFailed);
    }
    #[test]
    fn current_eligible_but_unchanged_attempt_is_a_conflict_not_storage_failure() {
        for (state, operation) in [
            (
                "inflight",
                complete_attempt as fn(&Connection, &str, &Value, &str) -> _,
            ),
            ("inflight", fail_attempt),
            ("failed", incomplete_attempt),
        ] {
            let db = fixture(state);
            if state == "failed" {
                db.execute("UPDATE model_attempt SET error_json=?1", [json!({"error":{"kind":"provider","message":"context limit","provider_code":"context_length_exceeded","status":400}}).to_string()]).unwrap();
            }
            db.execute_batch("CREATE TRIGGER suppress_attempt BEFORE UPDATE ON model_attempt BEGIN SELECT RAISE(IGNORE); END;").unwrap();
            let original = saved(&db);
            let error = operation(&db, "attempt", &json!({}), "now")
                .unwrap_err()
                .into_api_error();
            assert_eq!(error.code, ErrorCode::OperationConflict);
            assert_eq!(saved(&db), original);
        }
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
