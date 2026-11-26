//! Durable path-admission facts and recovery inside existing Control transactions.
//! path_admission is bounded scalar metadata, like receipt.state/hold_reason;
//! logical receipt accounting continues to count sender/result payloads only.
//! Recovery context payloads use the existing event byte accounting.
use crate::{
    history,
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

mod hold;
pub(crate) use hold::MessageHolds;

pub(super) fn plan_turn_input(
    db: &Connection,
    thread: &str,
    turn: &str,
    at: &str,
) -> rusqlite::Result<(Vec<String>, AdmissionPlan)> {
    let mut statement = db.prepare(
        "SELECT id,message_node_id FROM receipt WHERE turn_id=?1 AND state='accepted' ORDER BY created_at,rowid",
    )?;
    let rows = statement.query_map([turn], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let (receipts, nodes): (Vec<_>, Vec<_>) = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .unzip();
    let plan = plan_admission_tx(db, thread, &nodes, at)?;
    let admitted: std::collections::HashSet<_> = plan.admitted_callers.iter().collect();
    let receipts = receipts
        .into_iter()
        .zip(&nodes)
        .filter_map(|(receipt, node)| admitted.contains(node).then_some(receipt))
        .collect();
    Ok((receipts, plan))
}

pub(crate) fn held_hint(reason: Option<&str>) -> &'static str {
    match reason {
        Some("run_completion") => {
            "Wait for the target Run to finish; the message is already saved."
        }
        Some("recovery_wait") => {
            "Inspect the parked Turn with shellboy turn get --turn ID before deciding how to resume."
        }
        Some("stopping" | "previous_instance_interrupted" | "terminal_before_path_admission") => {
            "Inspect the target Thread with shellboy thread get --thread ID before resuming it."
        }
        _ => "Check shellboy operation get --operation ID before resending.",
    }
}

const TERMINAL: &str = "(EXISTS(SELECT 1 FROM run r WHERE r.id=receipt.run_id AND r.state IN ('completed','stopped','failed')) OR EXISTS(SELECT 1 FROM turn u WHERE u.id=receipt.turn_id AND u.state IN ('completed','stopped','failed')))";

/// Call only at an actual message-delivery path boundary, not generic history selection.
pub(crate) fn mark_admitted_tx(
    db: &Connection,
    thread: &str,
    nodes: &[String],
    at: &str,
) -> rusqlite::Result<()> {
    for node in nodes {
        // A pending duty saves when it entered the Thread: only an own ask sent
        // after this time can keep the duty open at Run end (#433).
        db.execute("UPDATE receipt SET path_admission='admitted',result_json=CASE WHEN json_extract(result_json,'$.reply.state')='pending' THEN json_set(result_json,'$.reply.admitted_at',?3) ELSE result_json END WHERE target_thread_id=?1 AND message_node_id=?2 AND path_admission<>'admitted' AND EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=?2)", params![thread,node,at])?;
        let operation: Option<String> = db.query_row("SELECT client_operation_id FROM receipt WHERE target_thread_id=?1 AND message_node_id=?2",params![thread,node],|r|r.get(0)).optional()?;
        if let Some(operation) = operation {
            super::submission_retention::collect_operation(db, &operation)?;
        }
    }
    Ok(())
}

/// Finalize one carried receipt after path admission, in the same transaction.
pub(super) fn mark_interruption_carry_tx(
    tx: &Transaction<'_>,
    (receipt, node): (&str, &str),
    (thread, run, turn, new_turn, at): (&str, &str, &str, &str, &str),
) -> Result<(), StorageError> {
    let context = json!({"run_id":run,"turn_id":new_turn,"reason":"carried_after_turn_interruption","interrupted_turn_id":turn,"admitted_at":at});
    let changed = tx.execute(
        "UPDATE receipt SET state='applied',hold_reason='carried_after_turn_interruption',updated_at=?2,result_json=json_set(COALESCE(result_json,'{}'),'$.delivery_context',json(?3)) WHERE id=?1 AND state='held' AND path_admission='admitted' AND EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?4 AND p.node_id=?5)",
        params![receipt, at, context.to_string(), thread, node],
    )?;
    if changed != 1 {
        return Err(StorageError::Schema(
            "carried receipt was not admitted to the selected path".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AdmissionPlan {
    /// Complete selected path prefix, including any eligible staged questions.
    pub(crate) admitted_nodes: Vec<String>,
    /// Caller-owned IDs in the prefix; only these rows may be acknowledged.
    pub(crate) admitted_callers: Vec<String>,
    /// Caller-owned IDs withheld behind the earliest blocked question.
    pub(crate) deferred_callers: Vec<String>,
    /// Exact caller nodes linked by persisted `reply_to` to a blocked question.
    pub(crate) deferred_reply_nodes: Vec<String>,
}

/// Plan the entire eligible question frontier before changing selected path or caller state.
pub(crate) fn plan_admission_tx(
    db: &Connection,
    thread: &str,
    caller_nodes: &[String],
    at: &str,
) -> rusqlite::Result<AdmissionPlan> {
    let mut ordered = Vec::<(String, i64, String)>::new();
    let mut seen = std::collections::HashSet::new();
    for node in caller_nodes {
        let (created,rowid):(String,i64)=db.query_row(
            "SELECT COALESCE(r.created_at,n.created_at),n.rowid FROM node n LEFT JOIN receipt r ON r.message_node_id=n.id AND r.target_thread_id=?1 WHERE n.id=?2",
            params![thread,node],|r|Ok((r.get(0)?,r.get(1)?)),
        )?;
        if seen.insert(node.clone()) {
            ordered.push((created, rowid, node.clone()));
        }
    }
    let frontier = ordered
        .last()
        .map(|(created, rowid, _)| (created.clone(), *rowid))
        .unwrap_or_else(|| (at.to_owned(), i64::MAX));
    let caller_set = serde_json::to_string(caller_nodes).expect("caller node IDs serialize");
    // Process a bounded source-order prefix plus one sentinel. If the sentinel
    // lies before the caller frontier, defer that caller suffix rather than
    // admitting a reply whose question fell outside this transaction's window.
    const QUESTION_PREFIX: usize = 129;
    let questions=db.prepare(
        "SELECT r.message_node_id, r.created_at, n.rowid FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 AND r.state='applied' AND r.path_admission='unadmitted' AND r.run_id IS NULL AND r.turn_id IS NULL AND json_extract(r.result_json,'$.to_human')=1 AND json_extract(r.result_json,'$.reply.state') IN ('pending','replied') ORDER BY r.created_at,n.rowid LIMIT 130",
    )?.query_map([thread],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut cutoff: Option<(String, i64)> = None;
    let mut deferred_reply_nodes = Vec::new();
    for (index, (question, created, rowid)) in questions.into_iter().enumerate() {
        let key = (created.clone(), rowid);
        if key > (frontier.0.clone(), frontier.1) {
            break;
        }
        if index == QUESTION_PREFIX {
            let replies:Vec<String>=db.prepare(
                "SELECT answer.message_node_id FROM receipt answer WHERE answer.target_thread_id=?1 AND json_extract(answer.result_json,'$.reply_to')=?2 AND answer.message_node_id IN (SELECT value FROM json_each(?3)) ORDER BY answer.created_at,answer.rowid",
            )?.query_map(params![thread,question,caller_set],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            deferred_reply_nodes.extend(replies);
            cutoff = Some(key);
            break;
        }
        if seen.contains(&question) {
            continue;
        }
        // Revisit is a later timing class, not a barrier to current input.
        let blocked:bool=db.query_row(
            "SELECT EXISTS(SELECT 1 FROM (SELECT r.message_node_id AS node_id,COALESCE(r.created_at,n.created_at) AS created_at,n.rowid AS source_rowid FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.target_thread_id=?1 AND r.state IN ('accepted','held') AND NOT (r.state='held' AND r.hold_reason IS 'run_completion') AND r.path_admission<>'admitted' AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=n.id) UNION ALL SELECT d.node_id AS node_id,n.created_at AS created_at,n.rowid AS source_rowid FROM attachment_delivery d JOIN node n ON n.id=d.node_id WHERE d.thread_id=?1 AND d.status IN ('accepted','held') AND NOT EXISTS(SELECT 1 FROM thread_path p WHERE p.thread_id=?1 AND p.node_id=d.node_id)) pending WHERE (pending.created_at<?2 OR (pending.created_at=?2 AND pending.source_rowid<?3)) AND pending.node_id NOT IN (SELECT value FROM json_each(?4)))",
            params![thread,created,rowid,caller_set],|r|r.get(0),
        )?;
        if blocked {
            // Canonical persisted reply_to identifies dependent replies in this caller batch.
            let replies:Vec<String>=db.prepare(
                "SELECT answer.message_node_id FROM receipt answer WHERE answer.target_thread_id=?1 AND json_extract(answer.result_json,'$.reply_to')=?2 AND answer.message_node_id IN (SELECT value FROM json_each(?3)) ORDER BY answer.created_at,answer.rowid",
            )?.query_map(params![thread,question,caller_set],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for reply in replies {
                if !deferred_reply_nodes.contains(&reply) {
                    deferred_reply_nodes.push(reply);
                }
            }
            cutoff = Some(key);
            break;
        }
        let position = ordered
            .iter()
            .position(|(at, id, _)| (at.as_str(), *id) > (created.as_str(), rowid))
            .unwrap_or(ordered.len());
        ordered.insert(position, (created, rowid, question.clone()));
        seen.insert(question);
    }
    let mut admitted_callers = Vec::new();
    let mut deferred_callers = Vec::new();
    let mut defer_suffix = false;
    if let Some((created, rowid)) = cutoff.as_ref() {
        for node in caller_nodes {
            let key:Option<(String,i64)>=db.query_row(
                "SELECT COALESCE(r.created_at,n.created_at),n.rowid FROM node n LEFT JOIN receipt r ON r.message_node_id=n.id AND r.target_thread_id=?1 WHERE n.id=?2",
                params![thread,node],|r|Ok((r.get(0)?,r.get(1)?)),
            ).optional()?;
            defer_suffix |=
                key.is_some_and(|(at, id)| (at.as_str(), id) >= (created.as_str(), *rowid));
            let destination = if defer_suffix {
                &mut deferred_callers
            } else {
                &mut admitted_callers
            };
            if !destination.contains(node) {
                destination.push(node.clone());
            }
        }
    } else {
        for node in caller_nodes {
            if !admitted_callers.contains(node) {
                admitted_callers.push(node.clone());
            }
        }
    }
    let admitted_set: std::collections::HashSet<&str> =
        admitted_callers.iter().map(String::as_str).collect();
    let admitted_nodes = ordered
        .iter()
        .filter(|(at, id, node)| {
            if caller_nodes.contains(node) {
                admitted_set.contains(node.as_str())
            } else {
                cutoff
                    .as_ref()
                    .is_none_or(|(created, rowid)| (at.as_str(), *id) < (created.as_str(), *rowid))
            }
        })
        .map(|(_, _, node)| node.clone())
        .collect();
    Ok(AdmissionPlan {
        admitted_nodes,
        admitted_callers,
        deferred_callers,
        deferred_reply_nodes,
    })
}

pub(crate) fn append_admission_plan_tx(
    tx: &Transaction<'_>,
    thread: &str,
    revision: u64,
    plan: &AdmissionPlan,
    at: &str,
) -> Result<(), StorageError> {
    if history::selection_revision(tx, thread, history::ThreadScope::Live)? as u64 != revision {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            "selected path revision changed; discard late adoption",
        )
        .into());
    }
    let nodes = plan.admitted_nodes.as_slice();
    history::append_existing_nodes(tx, thread, revision, nodes, at)?;
    mark_admitted_tx(tx, thread, nodes, at)?;
    Ok(())
}

pub(crate) fn append_admitted_tx(
    tx: &Transaction<'_>,
    thread: &str,
    revision: u64,
    nodes: &[String],
    at: &str,
) -> Result<AdmissionPlan, StorageError> {
    let plan = plan_admission_tx(tx, thread, nodes, at)?;
    append_admission_plan_tx(tx, thread, revision, &plan, at)?;
    Ok(plan)
}

/// Shared startup, stop-drain and fallback classification. Repeated observations
/// preserve the first saved reason/time; no assignment or immutable node changes.
pub(crate) fn repair_terminal_tx(db: &Connection, at: &str) -> rusqlite::Result<()> {
    db.execute("UPDATE receipt SET state='cancelled',hold_reason='target_or_source_unavailable',updated_at=?1 WHERE state IN ('accepted','held') AND (NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL))",[at])?;
    db.execute("UPDATE receipt SET hold_reason='uncertain_path_admission',updated_at=?1 WHERE state='held' AND path_admission='unknown' AND run_id IS NULL AND turn_id IS NULL AND hold_reason IS NOT 'uncertain_path_admission'",[at])?;
    db.execute(&format!("UPDATE receipt SET state=CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END,hold_reason=CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END,updated_at=?1 WHERE state IN ('accepted','held') AND {TERMINAL} AND (state<>CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END OR hold_reason IS NOT CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END)"),[at])?;
    Ok(())
}

/// Classify only one Thread's accepted/held receipts during an exact-run Stop.
/// Ordinary shutdown keeps the existing global startup/terminal repair sweep.
pub(crate) fn repair_terminal_thread_tx(
    db: &Connection,
    thread: &str,
    at: &str,
) -> rusqlite::Result<()> {
    db.execute("UPDATE receipt SET state='cancelled',hold_reason='target_or_source_unavailable',updated_at=?1 WHERE target_thread_id=?2 AND state IN ('accepted','held') AND (NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL))",params![at,thread])?;
    db.execute("UPDATE receipt SET hold_reason='uncertain_path_admission',updated_at=?1 WHERE target_thread_id=?2 AND state='held' AND path_admission='unknown' AND run_id IS NULL AND turn_id IS NULL AND hold_reason IS NOT 'uncertain_path_admission'",params![at,thread])?;
    db.execute(&format!("UPDATE receipt SET state=CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END,hold_reason=CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END,updated_at=?1 WHERE target_thread_id=?2 AND state IN ('accepted','held') AND {TERMINAL} AND (state<>CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END OR hold_reason IS NOT CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END)"),params![at,thread])?;
    Ok(())
}

/// Repair only this recipient's oldest 128 rows needing a state change.
/// The global startup repair must never run inside foreground reply admission.
fn repair_thread_batch_tx(db: &Connection, thread: &str, at: &str) -> rusqlite::Result<()> {
    let terminal = format!(
        "({TERMINAL} AND NOT EXISTS(SELECT 1 FROM run live_run WHERE live_run.id=receipt.run_id AND live_run.state IN ('live','stopping')))"
    );
    let rows = db.prepare(&format!("SELECT id FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state IN ('accepted','held') AND ((NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL)) OR (state='held' AND path_admission='unknown' AND run_id IS NULL AND turn_id IS NULL AND hold_reason IS NOT 'uncertain_path_admission') OR ({terminal} AND (state<>CASE WHEN path_admission='admitted' THEN 'applied' ELSE 'held' END OR hold_reason IS NOT CASE path_admission WHEN 'admitted' THEN 'terminal_after_path_admission' WHEN 'unadmitted' THEN 'terminal_before_path_admission' ELSE 'uncertain_path_admission' END))) ORDER BY created_at,rowid LIMIT 128"))?
        .query_map([thread], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for id in rows {
        db.execute(&format!("UPDATE receipt SET state=CASE WHEN NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL) THEN 'cancelled' WHEN {terminal} AND path_admission='admitted' THEN 'applied' ELSE 'held' END, hold_reason=CASE WHEN NOT EXISTS(SELECT 1 FROM thread t WHERE t.id=receipt.target_thread_id AND t.deleted_at IS NULL) OR NOT EXISTS(SELECT 1 FROM node n WHERE n.id=receipt.message_node_id AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL) THEN 'target_or_source_unavailable' WHEN path_admission='unknown' THEN 'uncertain_path_admission' WHEN {terminal} AND path_admission='admitted' THEN 'terminal_after_path_admission' ELSE 'terminal_before_path_admission' END,updated_at=?2 WHERE id=?1"), params![id,at])?;
    }
    Ok(())
}

pub(super) fn interrupted_remaining(db: &Connection, thread: &str) -> rusqlite::Result<bool> {
    let terminal = format!(
        "({TERMINAL} AND NOT EXISTS(SELECT 1 FROM run live_run WHERE live_run.id=receipt.run_id AND live_run.state IN ('live','stopping')))"
    );
    db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state IN ('accepted','held') AND path_admission='unadmitted' AND ({terminal} OR (run_id IS NULL AND turn_id IS NULL AND hold_reason IN ('stopping','previous_instance_interrupted'))))"), [thread], |r| r.get(0))
}

// The retry count and the batch selector describe exactly the same input.
fn recoverable_input() -> String {
    format!(
        "FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state IN ('accepted','held') AND ((path_admission='unadmitted' AND (({TERMINAL} AND NOT EXISTS(SELECT 1 FROM run live_run WHERE live_run.id=receipt.run_id AND live_run.state IN ('live','stopping'))) OR (run_id IS NULL AND turn_id IS NULL AND hold_reason IN ('stopping','previous_instance_interrupted')))) OR (run_id=?2 AND turn_id=?3 AND hold_reason='recovery_wait')) AND EXISTS(SELECT 1 FROM node source WHERE source.id=receipt.message_node_id AND source.payload_json IS NOT NULL AND source.deleted_reason IS NULL)"
    )
}

pub(super) fn retry_remaining(
    db: &Connection,
    thread: &str,
    run: &str,
    turn: &str,
) -> rusqlite::Result<i64> {
    db.query_row(
        &format!("SELECT count(*) {}", recoverable_input()),
        params![thread, run, turn],
        |r| r.get(0),
    )
}

/// Caller has established a new Run under human/live-owning-parent authority.
/// Existing message and receipt provenance stays fixed; only the selected path
/// and the original delivery outcome advance. All writes share send's transaction.
pub(crate) fn recover_on_resume_tx(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    actor: Value,
    at: &str,
) -> Result<usize, StorageError> {
    repair_thread_batch_tx(tx, thread, at)?;
    // Merge interrupted and current recovery input before applying LIMIT: neither
    // source may overtake an older receipt from the other source. Repair has
    // its own bounded window: eligible accepted rows beyond it must participate
    // directly, rather than waiting for repair while newer held rows pass them.
    let mut q = tx.prepare(&format!("SELECT id,message_node_id,run_id,turn_id,COALESCE(run_id=?2 AND turn_id=?3 AND hold_reason='recovery_wait',0) {} ORDER BY created_at,rowid LIMIT 128", recoverable_input()))?;
    let rows = q
        .query_map(params![thread, run, turn], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    // A reader's 128-row window is not an admission invariant. Each explicit
    // resume releases the oldest batch; later passes retain and drain the rest.
    if rows.is_empty() {
        return Ok(0);
    }
    let nodes = rows.iter().map(|row| row.1.clone()).collect::<Vec<_>>();
    let revision = history::selection_revision(tx, thread, history::ThreadScope::Live)?;
    let plan = plan_admission_tx(tx, thread, &nodes, at)?;
    let admitted_rows = rows
        .iter()
        .filter(|row| plan.admitted_callers.contains(&row.1))
        .collect::<Vec<_>>();
    for (receipt, node, old_run, old_turn, _) in &admitted_rows {
        if old_run.is_none() && old_turn.is_none() {
            tx.execute("UPDATE receipt SET run_id=?2,turn_id=?3,result_json=json_set(COALESCE(result_json,'{}'),'$.assignment','existing_turn','$.delivery_status','applied','$.run_id',?2,'$.turn_id',?3) WHERE id=?1 AND run_id IS NULL AND turn_id IS NULL",params![receipt,run,turn])?;
            tx.execute(
                "UPDATE node SET turn_id=?2 WHERE id=?1 AND turn_id IS NULL",
                params![node, turn],
            )?;
        }
    }
    append_admission_plan_tx(tx, thread, revision as u64, &plan, at)?;
    for (receipt, _, _, _, current_recovery) in &admitted_rows {
        tx.execute("UPDATE receipt SET state=CASE WHEN ?3 THEN 'accepted' ELSE 'applied' END,hold_reason=CASE WHEN ?3 THEN NULL ELSE 'recovered_at_explicit_resume' END,updated_at=?2 WHERE id=?1 AND state IN ('accepted','held') AND path_admission='admitted'",params![receipt,at,current_recovery])?;
    }
    if admitted_rows.is_empty() {
        return Ok(0);
    }
    let sources=admitted_rows.iter().map(|(receipt,node,old_run,old_turn,_)|json!({"receipt_id":receipt,"message_id":node,"original_run_id":old_run,"original_turn_id":old_turn})).collect::<Vec<_>>();
    super::context::record(
        tx,
        thread,
        "interrupted_messages_recovered",
        actor,
        json!({"run_id":run,"turn_id":turn,"sources":sources,"meaning":"Earlier interrupted instructions were admitted before the new message; original assignments remain unchanged"}),
        at,
    )?;
    Ok(admitted_rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','before'); INSERT INTO run(id,thread_id,state,created_at)VALUES('old','t','stopped','before'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('old-turn','t','old','stopped','before');").unwrap();
        db
    }
    fn input(db: &Connection, id: &str, admission: &str, at: &str) {
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'old-turn','user',?2,'{\"role\":\"user\",\"content\":[]}')",params![id,at]).unwrap();
        db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,path_admission)VALUES(?1,?1,'null','t',?1,'old','old-turn','held',?2,?3)",params![id,at,admission]).unwrap();
    }
    fn state(db: &Connection, id: &str) -> (String, String, String) {
        db.query_row(
            "SELECT state,hold_reason,updated_at FROM receipt WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }
    #[test]
    fn saved_message_context_reason_and_original_body_are_public_without_resend() {
        let db = fixture();
        input(&db, "saved-held", "unadmitted", "saved-at");
        db.execute("UPDATE node SET payload_json=?1 WHERE id='saved-held'",[json!({"role":"user","content":[{"type":"input_text","text":"synthetic retained instruction"}]}).to_string()]).unwrap();
        repair_terminal_tx(&db, "classified-at").unwrap();
        let before: String = db
            .query_row(
                "SELECT payload_json FROM node WHERE id='saved-held'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let details = crate::control::db::message_details(&db, "saved-held").unwrap();
        assert_eq!(
            details.delivery_status,
            crate::protocol::DeliveryStatus::Held
        );
        assert_eq!(
            details.context.hold_reason.as_deref(),
            Some("terminal_before_path_admission")
        );
        let args: crate::protocol::HistoryReadArgs =
            serde_json::from_value(json!({"node_id":"saved-held"})).unwrap();
        let body = history::read_node_body(&db, &args).unwrap();
        assert!(body.text.contains("synthetic retained instruction"));
        assert_eq!(
            db.query_row(
                "SELECT payload_json FROM node WHERE id='saved-held'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            before
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn exact_thread_terminal_repair_does_not_classify_an_unrelated_thread() {
        let db = fixture();
        input(&db, "current-admitted", "admitted", "0");
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('other','/','/a','p','m','before'); INSERT INTO run(id,thread_id,state,created_at)VALUES('other-run','other','stopped','before'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('other-turn','other','other-run','stopped','before'); INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('other-held','other-turn','user','1','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,path_admission)VALUES('other-held','other-op','null','other','other-held','other-run','other-turn','held','1','other-before','unadmitted');").unwrap();

        repair_terminal_thread_tx(&db, "t", "scoped-stop").unwrap();

        let current: (String, Option<String>, String) = db
            .query_row(
                "SELECT state,hold_reason,updated_at FROM receipt WHERE id='current-admitted'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            current,
            (
                "applied".into(),
                Some("terminal_after_path_admission".into()),
                "scoped-stop".into()
            )
        );
        let unrelated: (String, Option<String>, String) = db
            .query_row(
                "SELECT state,hold_reason,updated_at FROM receipt WHERE id='other-held'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(unrelated, ("held".into(), None, "other-before".into()));
    }

    #[test]
    fn terminal_repair_separates_admitted_unadmitted_unknown_and_preserves_first_observation() {
        let db = fixture();
        input(&db, "admitted", "admitted", "0");
        input(&db, "unseen", "unadmitted", "1");
        input(&db, "unknown", "unknown", "2");
        input(&db, "missing", "unadmitted", "3");
        db.execute(
            "UPDATE node SET payload_json=NULL,deleted_reason='collected' WHERE id='missing'",
            [],
        )
        .unwrap();
        repair_terminal_tx(&db, "repair").unwrap();
        assert_eq!(
            state(&db, "admitted"),
            (
                "applied".into(),
                "terminal_after_path_admission".into(),
                "repair".into()
            )
        );
        assert_eq!(
            state(&db, "unseen"),
            (
                "held".into(),
                "terminal_before_path_admission".into(),
                "repair".into()
            )
        );
        assert_eq!(
            state(&db, "unknown"),
            (
                "held".into(),
                "uncertain_path_admission".into(),
                "repair".into()
            )
        );
        assert_eq!(state(&db, "missing").0, "cancelled");
        repair_terminal_tx(&db, "later").unwrap();
        assert_eq!(state(&db, "unseen").2, "repair");
        assert_eq!(state(&db, "unknown").2, "repair");
        assert_eq!(state(&db, "admitted").2, "repair");
    }
    #[test]
    fn recovery_is_ordered_once_preserves_provenance_and_does_not_revive_removed_admitted_history()
    {
        let mut db = fixture();
        input(&db, "second", "unadmitted", "2");
        input(&db, "first", "unadmitted", "1");
        input(&db, "removed", "admitted", "0");
        input(&db, "uncertain", "unknown", "0");
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('unassigned','user','0.5','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,hold_reason)VALUES('unassigned','unassigned','null','t','unassigned','held','0.5','stopping');").unwrap();
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('new','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('new-turn','t','new','preparing','now');").unwrap();
        let tx = db.transaction().unwrap();
        assert_eq!(
            recover_on_resume_tx(&tx, "t", "new", "new-turn", json!({"kind":"human"}), "now")
                .unwrap(),
            3
        );
        assert_eq!(
            recover_on_resume_tx(
                &tx,
                "t",
                "new",
                "new-turn",
                json!({"kind":"human"}),
                "later"
            )
            .unwrap(),
            0
        );
        let nodes: Vec<String> = tx
            .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(nodes, ["unassigned", "first", "second"]);
        let assigned: (String, String) = tx
            .query_row(
                "SELECT run_id,turn_id FROM receipt WHERE id='unassigned'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(assigned, ("new".into(), "new-turn".into()));
        for id in ["first", "second"] {
            let saved:(String,String,String,String)=tx.query_row("SELECT r.run_id,r.turn_id,n.turn_id,r.path_admission FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE r.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            assert_eq!(
                saved,
                (
                    "old".into(),
                    "old-turn".into(),
                    "old-turn".into(),
                    "admitted".into()
                )
            );
            assert_eq!(state(&tx, id).1, "recovered_at_explicit_resume");
        }
        let events:i64=tx.query_row("SELECT count(*) FROM event WHERE json_extract(payload_ref_json,'$.kind')='interrupted_messages_recovered'",[],|r|r.get(0)).unwrap();
        assert_eq!(events, 1);
        assert_eq!(state(&tx, "uncertain").0, "held");
        assert_eq!(state(&tx, "removed").0, "applied");
        tx.commit().unwrap();
    }
    #[test]
    fn oversized_unavailable_sources_make_repair_progress_without_partial_admission() {
        let mut db = fixture();
        for n in 0..257 {
            input(&db, &format!("n{n:03}"), "unadmitted", "0");
        }
        db.execute(
            "UPDATE node SET payload_json=NULL,deleted_reason='collected'",
            [],
        )
        .unwrap();
        for cancelled in [128, 256, 257] {
            let tx = db.transaction().unwrap();
            assert_eq!(
                recover_on_resume_tx(&tx, "t", "new", "next", json!({"kind":"human"}), "resume")
                    .unwrap(),
                0
            );
            assert_eq!(
                tx.query_row(
                    "SELECT count(*) FROM receipt WHERE state='cancelled'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                cancelled
            );
            assert_eq!(
                tx.query_row("SELECT count(*) FROM thread_path", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            tx.commit().unwrap();
        }
        assert!(!interrupted_remaining(&db, "t").unwrap());
    }

    #[test]
    fn recipient_repair_writes_at_most_128_and_retains_unrelated_rows() {
        let db = fixture();
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('unrelated','/','/a','p','m','before')",[]).unwrap();
        for n in 0..257 {
            input(&db, &format!("n{n:03}"), "unadmitted", "0");
        }
        input(&db, "foreign", "unadmitted", "0");
        db.execute(
            "UPDATE receipt SET target_thread_id='unrelated' WHERE id='foreign'",
            [],
        )
        .unwrap();
        db.execute_batch("CREATE TEMP TABLE writes(id TEXT); CREATE TEMP TRIGGER audit_repair AFTER UPDATE ON receipt BEGIN INSERT INTO writes VALUES(new.id); END;").unwrap();
        for expected in [128, 128, 1, 0] {
            db.execute("DELETE FROM writes", []).unwrap();
            repair_thread_batch_tx(&db, "t", "repair").unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM writes", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                expected
            );
            assert_eq!(
                db.query_row(
                    "SELECT hold_reason FROM receipt WHERE id='foreign'",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )
                .unwrap(),
                None
            );
        }
    }

    #[test]
    fn accepted_recovery_beyond_repair_window_precedes_newer_held_input() {
        let mut db = fixture();
        for n in 0..128 {
            let id = format!("missing{n:03}");
            input(&db, &id, "unadmitted", "0");
            db.execute(
                "UPDATE node SET payload_json=NULL,deleted_reason='collected' WHERE id=?1",
                [&id],
            )
            .unwrap();
        }
        input(&db, "older-accepted", "unadmitted", "1");
        db.execute(
            "UPDATE receipt SET state='accepted' WHERE id='older-accepted'",
            [],
        )
        .unwrap();
        input(&db, "newer-held", "unadmitted", "2");
        let tx = db.transaction().unwrap();
        assert_eq!(
            recover_on_resume_tx(&tx, "t", "new", "next", json!({"kind":"human"}), "resume")
                .unwrap(),
            2
        );
        let selected = tx
            .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(selected, ["older-accepted", "newer-held"]);
        assert_eq!(state(&tx, "older-accepted").0, "applied");
        assert!(!interrupted_remaining(&tx, "t").unwrap());
        tx.commit().unwrap();
    }

    #[test]
    fn unrepaired_accepted_only_remainder_keeps_recovery_retryable() {
        let mut db = fixture();
        for n in 0..129 {
            let id = format!("missing{n:03}");
            input(&db, &id, "unadmitted", "0");
            db.execute("UPDATE receipt SET state='accepted' WHERE id=?1", [&id])
                .unwrap();
            db.execute(
                "UPDATE node SET payload_json=NULL,deleted_reason='collected' WHERE id=?1",
                [&id],
            )
            .unwrap();
        }
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('new','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES('next','t','new','recovery_wait','now','{}');").unwrap();
        for (generation, expected_state) in [(0, "recovery_wait"), (1, "preparing")] {
            let tx = db.transaction().unwrap();
            assert!(
                super::super::state::resume_recovery_turn_tx(
                    &tx, "t", "new", "next", generation, "resume"
                )
                .unwrap()
            );
            assert_eq!(
                tx.query_row("SELECT state FROM turn WHERE id='next'", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                expected_state
            );
            assert_eq!(interrupted_remaining(&tx, "t").unwrap(), generation == 0);
            tx.commit().unwrap();
        }
        assert_eq!(state(&db, "missing128").0, "cancelled");
    }

    #[test]
    fn mixed_recovery_sources_share_one_oldest_first_unit() {
        let mut db = fixture();
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('new','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('next','t','new','recovery_wait','now');").unwrap();
        for n in 0..257 {
            let id = format!("n{n:03}");
            if n % 2 == 0 {
                db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES(?1,'next','user',?2,'{}')", params![id,format!("{n:03}")]).unwrap();
                db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,path_admission,hold_reason)VALUES(?1,?1,'null','t',?1,'new','next','held',?2,'unadmitted','recovery_wait')",params![id,format!("{n:03}")]).unwrap();
            } else {
                input(&db, &id, "unadmitted", &format!("{n:03}"));
            }
        }
        for (generation, count) in [(0, 128), (1, 256), (2, 257)] {
            let tx = db.transaction().unwrap();
            assert!(
                super::super::state::resume_recovery_turn_tx(
                    &tx, "t", "new", "next", generation, "resume"
                )
                .unwrap()
            );
            let selected = tx
                .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                selected,
                (0..count).map(|n| format!("n{n:03}")).collect::<Vec<_>>()
            );
            assert_eq!(
                tx.query_row("SELECT count(*) FROM receipt WHERE state='held'", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                257 - count
            );
            tx.commit().unwrap();
        }
    }

    #[test]
    fn recovery_passes_preserve_order_restart_and_stale_retry_safety() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery.db");
        let db = fixture();
        for n in 0..257 {
            input(&db, &format!("n{n:03}"), "unadmitted", "0");
        }
        db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('new','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json)VALUES('next','t','new','recovery_wait','now','{}');").unwrap();
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()])
            .unwrap();
        drop(db);
        for (generation, applied, expected_state) in [
            (0, 128, "recovery_wait"),
            (1, 256, "recovery_wait"),
            (2, 257, "preparing"),
        ] {
            let mut db = Connection::open(&path).unwrap();
            db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
            let tx = db.transaction().unwrap();
            assert!(
                super::super::state::resume_recovery_turn_tx(
                    &tx, "t", "new", "next", generation, "resume"
                )
                .unwrap()
            );
            assert!(
                !super::super::state::resume_recovery_turn_tx(
                    &tx,
                    "t",
                    "new",
                    "next",
                    generation,
                    "stale concurrent retry",
                )
                .unwrap()
            );
            assert_eq!(
                tx.query_row(
                    "SELECT count(*) FROM receipt WHERE state='applied'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                applied
            );
            assert_eq!(
                tx.query_row("SELECT state FROM turn WHERE id='next'", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                expected_state
            );
            let selected = tx
                .prepare("SELECT node_id FROM thread_path ORDER BY ordinal")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                selected,
                (0..applied).map(|n| format!("n{n:03}")).collect::<Vec<_>>()
            );
            tx.commit().unwrap();
        }
    }

    #[test]
    fn bounded_recovery_and_partial_path_failure_roll_back_every_change() {
        let mut db = fixture();
        for n in 0..129 {
            input(&db, &format!("n{n:03}"), "unadmitted", "0");
        }
        let tx = db.transaction().unwrap();
        assert_eq!(
            recover_on_resume_tx(&tx, "t", "new", "u", json!({"kind":"human"}), "now").unwrap(),
            128
        );
        assert_eq!(
            tx.query_row("SELECT state FROM receipt WHERE id='n128'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "held"
        );
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute("DELETE FROM receipt WHERE id='n128'", [])
            .unwrap();
        db.execute_batch("CREATE TRIGGER fail_recovery BEFORE INSERT ON thread_path WHEN new.node_id='n001' BEGIN SELECT RAISE(ABORT,'synthetic path failure'); END;").unwrap();
        let tx = db.transaction().unwrap();
        assert!(
            recover_on_resume_tx(&tx, "t", "new", "u", json!({"kind":"human"}), "now").is_err()
        );
        tx.rollback().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM receipt WHERE state='applied'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn startup_stop_drain_and_shutdown_share_terminal_classification() {
        for boundary in ["startup", "stop", "shutdown"] {
            let mut db = fixture();
            input(&db, "pending", "unadmitted", "0");
            input(&db, "seen", "admitted", "1");
            db.execute_batch("UPDATE run SET state='live'; UPDATE turn SET state='running'; UPDATE receipt SET state='accepted';").unwrap();
            let tx = db.transaction().unwrap();
            match boundary {
                "startup" => {
                    crate::control::state::interrupt_previous_work(&tx, "end", false).unwrap();
                }
                "stop" => crate::control::state::stop_drained_runs(
                    &tx,
                    &[("old".into(), "t".into())],
                    false,
                    &mut Vec::new(),
                )
                .unwrap(),
                _ => crate::control::state::finish_drained_shutdown(&tx, false).unwrap(),
            }
            assert_eq!(state(&tx, "pending").0, "held", "{boundary}");
            assert_eq!(
                state(&tx, "pending").1,
                "terminal_before_path_admission",
                "{boundary}"
            );
            assert_eq!(state(&tx, "seen").0, "applied", "{boundary}");
            tx.commit().unwrap();
        }
    }
    #[test]
    fn admission_fact_requires_real_target_path_and_survives_path_removal() {
        let mut db = fixture();
        input(&db, "input", "unadmitted", "0");
        mark_admitted_tx(&db, "t", &["input".into()], "now").unwrap();
        assert_eq!(
            db.query_row("SELECT path_admission FROM receipt", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "unadmitted"
        );
        let tx = db.transaction().unwrap();
        history::append_existing_nodes(&tx, "t", 0, &["input".into()], "now").unwrap();
        mark_admitted_tx(&tx, "t", &["input".into()], "now").unwrap();
        tx.execute("DELETE FROM thread_path", []).unwrap();
        repair_terminal_tx(&tx, "stopped").unwrap();
        assert_eq!(state(&tx, "input").0, "applied");
        tx.commit().unwrap();
    }
}
