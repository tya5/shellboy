//! Exact-message admission and owner-side foreground observation.
use super::*;
use crate::protocol::MessageHeldBy;
use rusqlite::{OptionalExtension, params};

impl Control {
    pub(super) async fn message_with_hold(
        self: &Arc<Self>,
        id: &str,
    ) -> Result<MutationResult, ApiError> {
        let target = id.to_owned();
        let owner = Arc::clone(self);
        let detail = self.store.database.call(move |db| {
            let tx = db.transaction()?;
            let mut detail = match db::message_details(&tx, &target) {
                Ok(detail) => detail,
                Err(error) => return Ok(Err(error)),
            };
            detail.held_by = owner.read_message_hold(&tx, &target)?;
            Ok(Ok(detail))
        }).await.map_err(storage_error)??;
        MutationResult::value(detail)
    }

    /// Used by native reads and live inspection on their existing DB snapshot.
    /// A missing, contended or conflicting binding never supplies a target.
    pub(crate) fn read_message_hold(
        &self,
        db: &rusqlite::Connection,
        message: &str,
    ) -> rusqlite::Result<Option<MessageHeldBy>> {
        let row = db.query_row(
            "SELECT target_thread_id,run_id,turn_id,state,hold_reason,
             COALESCE(json_extract(result_json,'$.to_human')=1,0)
             FROM receipt WHERE message_node_id=?1",
            [message], |r| Ok((r.get::<_,String>(0)?, r.get::<_,Option<String>>(1)?,
                r.get::<_,Option<String>>(2)?, r.get::<_,String>(3)?, r.get::<_,Option<String>>(4)?, r.get::<_,bool>(5)?)),
        ).optional()?;
        let Some((thread, run, turn, state, reason, false)) = row else { return Ok(None); };
        if state == "held" {
            return Ok(Some(match reason.as_deref() {
                Some("run_completion") => MessageHeldBy::RunCompletion,
                Some("recovery_wait") => MessageHeldBy::RecoveryWait,
                Some("stopping") => MessageHeldBy::Stopping,
                _ => MessageHeldBy::Unknown,
            }));
        }
        if state != "accepted" { return Ok(None); }
        let (Some(run), Some(turn)) = (run, turn) else { return Ok(Some(MessageHeldBy::Unknown)); };
        if !message_delivery::exact_input_candidate(db, &thread, &turn, message)? {
            return Ok(Some(MessageHeldBy::Unknown));
        }
        let Ok(execution) = self.live.foreground_observation(&thread, &run) else {
            return Ok(Some(MessageHeldBy::Unknown));
        };
        phase(db, &thread, &run, &turn, execution)
    }
}

fn phase(db: &rusqlite::Connection, thread: &str, run: &str, turn: &str, execution: Option<String>) -> rusqlite::Result<Option<MessageHeldBy>> {
    let phase = db.query_row(
        "SELECT u.state,r.state,u.interrupt_requested_at IS NOT NULL,
         EXISTS(SELECT 1 FROM shell_execution s
           JOIN tool_call c ON c.operation_id=s.id JOIN tool_group g ON g.id=c.group_id
           JOIN model_attempt a ON a.id=g.attempt_id
           WHERE s.id=?4 AND s.turn_id=u.id AND s.owner_run_id=r.id AND s.state='running'
           AND c.state='running' AND g.state='running' AND a.generation=u.generation),
         EXISTS(SELECT 1 FROM model_attempt a WHERE a.turn_id=u.id AND a.generation=u.generation
           AND a.kind='conversation' AND a.state='inflight')
         FROM turn u JOIN run r ON r.id=u.run_id JOIN thread t ON t.id=r.thread_id
         WHERE u.id=?1 AND r.id=?2 AND t.id=?3 AND t.deleted_at IS NULL",
        params![turn, run, thread, execution], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?, r.get::<_, bool>(3)?, r.get::<_, bool>(4)?)),
    ).optional()?;
    let Some((turn_state, run_state, interrupted, shell, generating)) = phase else {
        return Ok(Some(MessageHeldBy::Unknown));
    };
    Ok(match (run_state.as_str(), turn_state.as_str(), interrupted, execution, shell, generating) {
        ("stopping", _, _, _, _, _) => Some(MessageHeldBy::Stopping),
        ("live", "recovery_wait", false, None, false, false) => Some(MessageHeldBy::RecoveryWait),
        ("live", "running" | "waiting", false, Some(execution_id), true, false) => Some(MessageHeldBy::ForegroundShell { execution_id }),
        ("live", "running", false, None, false, true) => Some(MessageHeldBy::ResponseGeneration),
        ("live", "preparing" | "running" | "waiting", false, None, false, false) => None,
        _ => Some(MessageHeldBy::Unknown),
    })
}
