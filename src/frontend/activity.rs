//! One read projection for the existing progress cadence; no new core state.
use crate::{
    control::Control,
    protocol::ApiError,
    ui::{
        UiUpdate,
        activity::{ActivityPhase, ActivitySnapshot, AutomaticRetrySnapshot, RecoverySnapshot},
    },
};
use rusqlite::{Connection, OptionalExtension};

#[derive(Clone, Debug)]
pub struct Projection {
    pub snapshot: ActivitySnapshot,
    pub observed_at: std::time::Instant,
    pub tool_calls: u64,
    pub cancelled_calls: u64,
    pub poll_running: bool,
}
// Read-only rows, kept local to the frontend projection.
struct SavedTurn {
    id: String,
    state: String,
    generation: i64,
    recovery_reason: Option<String>,
}
struct SavedAttempt {
    id: String,
    state: String,
    generation: i64,
    error: Option<String>,
    automatic_retry: Option<String>,
    automatic_retry_exhausted: Option<String>,
}
/// Match only complete diagnostics constructed from the typed saved facts.
/// Provider text and the durable attempt remain unchanged.
fn model_failure_message(
    message: &str,
    model: &crate::model::ModelError,
    exhausted: Option<&str>,
) -> Option<String> {
    if message == model.to_string() {
        return Some(model.message.clone());
    }
    #[derive(serde::Deserialize)]
    struct Exhausted {
        retries: u32,
        limit: u32,
    }
    let counts: Exhausted = serde_json::from_str(exhausted?).ok()?;
    if !(1..=5).contains(&counts.limit) || counts.retries < counts.limit || counts.retries > 5 {
        return None;
    }
    let suffix = format!(
        " (automatic retries exhausted: {}/{})",
        counts.retries, counts.limit
    );
    (message == format!("{model}{suffix}")).then(|| format!("{}{suffix}", model.message))
}
impl Projection {
    pub fn label(&self) -> &'static str {
        self.snapshot.phase.label()
    }

    pub(super) fn into_activity_update(
        self,
        thread: &str,
        generation: u64,
        epoch: u64,
    ) -> UiUpdate {
        UiUpdate::Activity {
            thread_id: thread.into(),
            generation,
            epoch,
            snapshot: self.snapshot,
            observed_at: self.observed_at,
        }
    }
    pub fn progress_update(&self, thread: &str) -> UiUpdate {
        UiUpdate::Progress {
            thread_id: thread.into(),
            tool_calls: self.tool_calls,
            cancelled_calls: self.cancelled_calls,
            state: self.label().into(),
        }
    }
}
/// Latest Run owns the selected Turn; an older Turn cannot revive a newer Run.
/// A single DB worker closure observes IDs, phases, and counts together.
pub fn project(db: &Connection, thread: &str) -> Result<Projection, rusqlite::Error> {
    let run: Option<(String, String, Option<String>)> = db
        .query_row(
            "SELECT id,state,stop_reason FROM run WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1",
            [thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((run_id, run_state, stop_reason)) = run else {
        return Ok(Projection {
            snapshot: ActivitySnapshot::default(),
            observed_at: std::time::Instant::now(),
            tool_calls: 0,
            cancelled_calls: 0,
            poll_running: false,
        });
    };
    let turn = db.query_row("SELECT id,state,generation,recovery_reason_json FROM turn WHERE run_id=?1 ORDER BY rowid DESC LIMIT 1",[&run_id],|r|Ok(SavedTurn { id:r.get(0)?,state:r.get(1)?,generation:r.get(2)?,recovery_reason:r.get(3)? })).optional()?;
    let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')) OR EXISTS(SELECT 1 FROM run WHERE owner_run_id=?1 AND state IN ('live','stopping'))",[&run_id],|r|r.get(0))?;
    let mut snapshot = ActivitySnapshot {
        run_id: Some(run_id),
        ..Default::default()
    };
    let mut tools = 0;
    let mut cancelled = 0;
    let mut state = None;
    let mut attempt_state = None;
    let mut tool_running = false;
    if let Some(SavedTurn {
        id: turn_id,
        state: turn_state,
        generation,
        recovery_reason: reason,
    }) = turn
    {
        let attempt = db
            .query_row(
                "SELECT id,state,generation,CASE WHEN ?2 AND json_valid(error_json) THEN json_extract(error_json,'$.error') ELSE NULL END,CASE WHEN json_valid(error_json) THEN json_extract(error_json,'$.automatic_retry') ELSE NULL END,CASE WHEN json_valid(error_json) THEN CASE WHEN json_type(error_json,'$.automatic_retry_exhausted')='object' THEN json_extract(error_json,'$.automatic_retry_exhausted') ELSE NULL END ELSE NULL END FROM model_attempt WHERE turn_id=?1 ORDER BY rowid DESC LIMIT 1",
                rusqlite::params![turn_id,reason.is_some()],
                |r| Ok(SavedAttempt { id:r.get(0)?,state:r.get(1)?,generation:r.get(2)?,error:r.get(3)?,automatic_retry:r.get(4)?,automatic_retry_exhausted:r.get(5)? }),
            )
            .optional()?;
        if let Some(reason) = reason
            && (turn_state == "recovery_wait" || serde_json::from_str::<ApiError>(&reason).is_ok())
        {
            // Startup interruption writes a different saved reason schema. Its
            // presence alone is not evidence that this Turn awaited recovery.
            let error = serde_json::from_str::<ApiError>(&reason).ok();
            let mut message = error
                .as_ref()
                .map(|error| error.message.clone())
                .unwrap_or_else(|| "保存された復旧理由を詳細で確認してください".into());
            let mut stage = "処理（段階の記録なし）".to_owned();
            if let Some(SavedAttempt {
                generation: attempt_generation,
                state: attempt_saved_state,
                error: Some(body),
                automatic_retry_exhausted,
                ..
            }) = &attempt
                && *attempt_generation == generation
                && attempt_saved_state == "incomplete"
                && let Ok(model) = serde_json::from_str::<crate::model::ModelError>(body)
                && let Some(projected_message) =
                    model_failure_message(&message, &model, automatic_retry_exhausted.as_deref())
            {
                // This exact typed match proves the prefix is our ModelError
                // diagnostic kind; retain canonical JSON and project only the message.
                message = projected_message;
                stage = if model.is_context_limit() {
                    "モデル要求（入力容量超過）"
                } else {
                    "モデル要求"
                }
                .into();
            }
            let (pending_messages,saved_tool_results,live_shells,uncollected_shells) = db.query_row(
                "SELECT
                  (SELECT COUNT(*) FROM receipt WHERE target_thread_id=?1 AND state IN ('accepted','held')),
                  (SELECT COUNT(*) FROM tool_result r JOIN tool_group g ON g.id=r.group_id WHERE g.turn_id=?2),
                  (SELECT COUNT(*) FROM shell_execution WHERE owner_run_id=?3 AND state IN ('starting','running','stopping')),
                  (SELECT COUNT(*) FROM shell_execution WHERE owner_run_id=?3 AND state IN ('exited','failed','stopped') AND output_complete=0)",
                rusqlite::params![thread,turn_id,snapshot.run_id],
                |row| Ok((row.get::<_,i64>(0)? as u64,row.get::<_,i64>(1)? as u64,row.get::<_,i64>(2)? as u64,row.get::<_,i64>(3)? as u64)),
            )?;
            snapshot.recovery = Some(RecoverySnapshot {
                stage,
                message,
                pending_messages,
                saved_tool_results,
                live_shells,
                uncollected_shells,
                stop_reason: stop_reason.clone(),
            });
        }
        if let Some(SavedAttempt {
            id,
            state: saved_state,
            generation: attempt_generation,
            automatic_retry,
            ..
        }) = attempt
        {
            if run_state == "live"
                && turn_state == "running"
                && saved_state == "failed"
                && attempt_generation == generation
            {
                snapshot.automatic_retry = automatic_retry
                    .and_then(|body| serde_json::from_str::<AutomaticRetrySnapshot>(&body).ok())
                    .filter(|retry| retry.number > 0 && retry.number <= retry.limit);
            }
            snapshot.attempt_id = Some(id);
            attempt_state = Some(saved_state);
        }
        tool_running = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_group WHERE turn_id=?1 AND state='running')",
            [&turn_id],
            |r| r.get(0),
        )?;
        tools=db.query_row("SELECT COUNT(*) FROM shell_execution WHERE turn_id=?1 AND json_extract(shell_json,'$.spawned')=1",[&turn_id],|r|r.get::<_,i64>(0).map(|n|n as u64))?;
        cancelled=db.query_row("SELECT COUNT(*) FROM tool_result tr JOIN tool_group g ON g.id=tr.group_id WHERE g.turn_id=?1 AND tr.status='cancelled'",[&turn_id],|r|r.get::<_,i64>(0).map(|n|n as u64))?;
        snapshot.turn_in_progress = matches!(run_state.as_str(), "live" | "stopping")
            && matches!(turn_state.as_str(), "preparing" | "running" | "waiting");
        snapshot.turn_id = Some(turn_id);
        state = Some(turn_state);
    }
    snapshot.phase = match (run_state.as_str(), state.as_deref()) {
        ("stopping", _) => ActivityPhase::Stopping,
        ("failed", _) => ActivityPhase::Failed,
        ("stopped", _) => ActivityPhase::Stopped,
        ("completed", _) => ActivityPhase::Completed,
        (_, Some("recovery_wait")) => ActivityPhase::Recovery,
        (_, Some("preparing")) => ActivityPhase::Preparing,
        (_, Some("waiting")) => ActivityPhase::Waiting,
        (_, Some("running")) if attempt_state.as_deref() == Some("inflight") => {
            ActivityPhase::AwaitingModel
        }
        ("live", Some("running")) if snapshot.automatic_retry.is_some() => ActivityPhase::Retrying,
        (_, Some("running")) if tool_running => ActivityPhase::Tools,
        (_, Some("running")) => ActivityPhase::Running,
        ("live", Some("completed")) if owned => ActivityPhase::Background,
        (_, Some("failed")) => ActivityPhase::Failed,
        (_, Some("stopped")) => ActivityPhase::Stopped,
        ("live", _) => ActivityPhase::Continuing,
        _ => ActivityPhase::Completed,
    };
    let poll_running = run_state == "stopping"
        || (run_state == "live"
            && (owned
                || matches!(
                    state.as_deref(),
                    Some("preparing" | "running" | "waiting" | "recovery_wait")
                )));
    Ok(Projection {
        snapshot,
        observed_at: std::time::Instant::now(),
        tool_calls: tools,
        cancelled_calls: cancelled,
        poll_running,
    })
}
pub async fn read(control: &Control, thread: &str) -> Result<Projection, ApiError> {
    let target = thread.to_owned();
    let result = control
        .store
        .database
        .call(move |db| Ok(project(db, &target)?))
        .await
        .map_err(super::storage_error);
    super::checked_storage(control, result).await
}
