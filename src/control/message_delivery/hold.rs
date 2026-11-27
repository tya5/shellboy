//! Exact-message admission and owner-side foreground observation.
use super::super::*;
use crate::control::state::now;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(crate) struct MessageHolds(Arc<LiveRegistry>);
type TurnDecisions = HashMap<(String, String, String), (HashSet<String>, Option<MessageHeldBy>)>;
use crate::protocol::MessageHeldBy;
use rusqlite::{OptionalExtension, params};

impl Control {
    pub(crate) fn message_holds(&self) -> MessageHolds {
        MessageHolds(self.live.clone())
    }

    pub(in crate::control) async fn message_with_hold(
        self: &Arc<Self>,
        args: crate::protocol::MessageTarget,
    ) -> Result<MutationResult, ApiError> {
        // A pure read: the owner's read-only inspection lane, never the writer.
        self.store.observe_message(args, self.message_holds()).await
    }
}

impl MessageHolds {
    #[cfg(test)]
    pub(in crate::control) fn fixture() -> Self {
        Self(Arc::new(LiveRegistry::default()))
    }

    /// Project only the supplied rows, reusing the driver's ordered admission
    /// plan and one live binding observation per Turn in this read snapshot.
    pub(crate) fn read<'a>(
        &self,
        db: &rusqlite::Connection,
        messages: impl IntoIterator<Item = &'a str>,
    ) -> rusqlite::Result<Vec<Option<MessageHeldBy>>> {
        let mut turns = TurnDecisions::new();
        messages
            .into_iter()
            .map(|id| self.read_one(db, id, &mut turns))
            .collect()
    }

    fn read_one(
        &self,
        db: &rusqlite::Connection,
        message: &str,
        turns: &mut TurnDecisions,
    ) -> rusqlite::Result<Option<MessageHeldBy>> {
        let row = db
            .query_row(
                "SELECT target_thread_id,run_id,turn_id,state,hold_reason,
             COALESCE(json_extract(result_json,'$.to_human')=1,0)
             FROM receipt WHERE message_node_id=?1",
                [message],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((thread, run, turn, state, reason, false)) = row else {
            return Ok(None);
        };
        if state == "held" {
            return Ok(Some(match reason.as_deref() {
                Some("run_completion") => MessageHeldBy::RunCompletion,
                Some("recovery_wait") => MessageHeldBy::RecoveryWait,
                Some("stopping") => MessageHeldBy::Stopping,
                Some("previous_instance_interrupted") => MessageHeldBy::PreviousInstanceInterrupted,
                Some("terminal_before_path_admission") => {
                    MessageHeldBy::TerminalBeforePathAdmission
                }
                Some("uncertain_path_admission") => MessageHeldBy::UncertainPathAdmission,
                _ => MessageHeldBy::Unknown,
            }));
        }
        if state != "accepted" {
            return Ok(None);
        }
        let (Some(run), Some(turn)) = (run, turn) else {
            return Ok(Some(MessageHeldBy::Unknown));
        };
        use std::collections::hash_map::Entry;
        let decision = match turns.entry((thread, run, turn)) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let (thread, run, turn) = entry.key();
                let (_, plan) = super::plan_turn_input(db, thread, turn, &now())?;
                let blocker = match self.0.foreground_observation(thread, run) {
                    Ok(execution) => phase(db, thread, run, turn, execution)?,
                    Err(()) => Some(MessageHeldBy::Unknown),
                };
                entry.insert((plan.admitted_callers.into_iter().collect(), blocker))
            }
        };
        Ok(if decision.0.contains(message) {
            decision.1.clone()
        } else {
            Some(MessageHeldBy::Unknown)
        })
    }
}

fn phase(
    db: &rusqlite::Connection,
    thread: &str,
    run: &str,
    turn: &str,
    execution: Option<String>,
) -> rusqlite::Result<Option<MessageHeldBy>> {
    let phase = db
        .query_row(
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
            params![turn, run, thread, execution],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((turn_state, run_state, interrupted, shell, generating)) = phase else {
        return Ok(Some(MessageHeldBy::Unknown));
    };
    Ok(
        match (
            run_state.as_str(),
            turn_state.as_str(),
            interrupted,
            execution,
            shell,
            generating,
        ) {
            ("stopping", _, _, _, _, _) => Some(MessageHeldBy::Stopping),
            ("live", "recovery_wait", false, None, false, false) => {
                Some(MessageHeldBy::RecoveryWait)
            }
            ("live", "running" | "waiting", false, Some(execution_id), true, false) => {
                Some(MessageHeldBy::ForegroundShell { execution_id })
            }
            ("live", "running", false, None, false, true) => {
                Some(MessageHeldBy::ResponseGeneration)
            }
            ("live", "preparing" | "running" | "waiting", false, None, false, false) => None,
            _ => Some(MessageHeldBy::Unknown),
        },
    )
}
