//! Current, exact blocker for an individual pending model message.
use super::*;
use crate::protocol::{DeliveryStatus, MessageDetails, MessageHeldBy};

impl Control {
    pub(super) async fn message_with_hold(&self, id: &str) -> Result<MutationResult, ApiError> {
        let target = id.to_owned();
        let first = self
            .store
            .database
            .call(move |db| Ok(db::message_details(db, &target)))
            .await
            .map_err(storage_error)??;
        let (Some(run), Some(turn)) = (&first.run_id, &first.turn_id) else {
            return finish(first, None);
        };
        if !eligible(&first) {
            return finish(first, None);
        }
        // Bracket the saved Turn/generation check with observations of the same slot.
        let slot = self.live.foreground_for_run(&first.thread_id, run).await;
        let execution = slot.as_ref().and_then(|slot| slot.waiting_execution());
        let target = id.to_owned();
        let run = run.clone();
        let turn = turn.clone();
        let observed_execution = execution.clone();
        let (fresh, mut blocker) = self
            .store
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let fresh = match db::message_details(&tx, &target) {
                    Ok(details) => details,
                    Err(error) => return Ok(Err(error)),
                };
                let blocker = if eligible(&fresh)
                    && fresh.run_id.as_deref() == Some(&run)
                    && fresh.turn_id.as_deref() == Some(&turn)
                {
                    read_blocker(
                        &tx,
                        &fresh.thread_id,
                        &run,
                        &turn,
                        observed_execution.as_deref(),
                    )?
                } else {
                    None
                };
                Ok(Ok((fresh, blocker)))
            })
            .await
            .map_err(storage_error)??;
        if let Some(run) = &fresh.run_id
            && matches!(blocker, Some(MessageHeldBy::ForegroundShell { .. }))
        {
            let current = self.live.foreground_for_run(&fresh.thread_id, run).await;
            if !matches!((&slot, &current), (Some(before), Some(after)) if Arc::ptr_eq(before, after) && after.waiting_execution() == execution)
            {
                blocker = None;
            }
        }
        finish(fresh, blocker)
    }
}

fn eligible(message: &MessageDetails) -> bool {
    !message.to_human && message.delivery_status == DeliveryStatus::Accepted
}

fn finish(
    mut message: MessageDetails,
    live: Option<MessageHeldBy>,
) -> Result<MutationResult, ApiError> {
    message.held_by = if message.delivery_status == DeliveryStatus::Held {
        Some(match message.context.hold_reason.as_deref() {
            Some("run_completion") => MessageHeldBy::RunCompletion,
            Some("recovery_wait") => MessageHeldBy::RecoveryWait,
            Some("stopping") => MessageHeldBy::Stopping,
            _ => MessageHeldBy::Unknown,
        })
    } else {
        live
    };
    MutationResult::value(message)
}

fn read_blocker(
    db: &rusqlite::Connection,
    thread: &str,
    run: &str,
    turn: &str,
    execution: Option<&str>,
) -> rusqlite::Result<Option<MessageHeldBy>> {
    let (shell_matches, generation_active): (bool, bool) = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id
          JOIN shell_execution s ON s.turn_id=u.id AND s.owner_run_id=r.id
          JOIN tool_call c ON c.operation_id=s.id JOIN tool_group g ON g.id=c.group_id
          JOIN model_attempt a ON a.id=g.attempt_id
          WHERE u.id=?1 AND r.id=?2 AND r.thread_id=?3 AND s.id=?4 AND r.state='live'
          AND u.state IN ('running','waiting') AND u.interrupt_requested_at IS NULL
          AND s.state='running' AND c.state='running' AND g.state='running' AND a.generation=u.generation),
         EXISTS(SELECT 1 FROM turn u JOIN run r ON r.id=u.run_id JOIN model_attempt a ON a.turn_id=u.id
          WHERE u.id=?1 AND r.id=?2 AND r.thread_id=?3 AND r.state='live' AND u.state='running'
          AND u.interrupt_requested_at IS NULL AND a.generation=u.generation AND a.kind='conversation' AND a.state='inflight')",
        rusqlite::params![turn, run, thread, execution], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(match (execution, shell_matches, generation_active) {
        (Some(id), true, false) => Some(MessageHeldBy::ForegroundShell {
            execution_id: id.to_owned(),
        }),
        (None, false, true) => Some(MessageHeldBy::ResponseGeneration),
        _ => None,
    })
}
