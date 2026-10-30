//! One read projection for the existing progress cadence; no new core state.
mod reduction;
use crate::{
    control::Control,
    protocol::ApiError,
    ui::{
        UiUpdate,
        activity::{ActivitySnapshot, AutomaticRetrySnapshot, RecoverySnapshot},
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
    if !(1..=20).contains(&counts.limit) || counts.retries < counts.limit || counts.retries > 20 {
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
/// Owned SQL facts from one caller-controlled read transaction. Projection and
/// model-error/diagnostic decoding are deliberately outside the DB worker.
pub(super) struct RawProjection {
    run: Option<(String, String, Option<String>)>,
    turn: Option<SavedTurn>,
    attempt: Option<SavedAttempt>,
    revisit: Option<String>,
    context_reduction: Option<String>,
    recovery_counts: Option<(u64, u64, u64, u64)>,
    owned: bool,
    tool_running: bool,
    tools: u64,
    cancelled: u64,
    live_work: Option<super::live_work::RawLiveWork>,
}

/// Latest Run owns the selected Turn; an older Turn cannot revive a newer Run.
pub(super) fn read_tx(
    db: &Connection,
    thread: &str,
) -> Result<RawProjection, crate::storage::StorageError> {
    read_tx_with_waits(db, thread, None)
}

pub(super) fn read_tx_with_waits(
    db: &Connection,
    thread: &str,
    waits: Option<&[(String, String)]>,
) -> Result<RawProjection, crate::storage::StorageError> {
    let run: Option<(String, String, Option<String>)> = db
        .query_row(
            "SELECT id,state,stop_reason FROM run WHERE thread_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",
            [thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((ref run_id, ref run_state, _)) = run else {
        return Ok(RawProjection {
            run,
            turn: None,
            attempt: None,
            revisit: None,
            context_reduction: None,
            recovery_counts: None,
            owned: false,
            tool_running: false,
            tools: 0,
            cancelled: 0,
            live_work: None,
        });
    };
    let turn = db.query_row("SELECT id,state,generation,recovery_reason_json FROM turn WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[run_id],|r|Ok(SavedTurn { id:r.get(0)?,state:r.get(1)?,generation:r.get(2)?,recovery_reason:r.get(3)? })).optional()?;
    let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')) OR EXISTS(SELECT 1 FROM run WHERE owner_run_id=?1 AND state IN ('live','stopping'))",[run_id],|r|r.get(0))?;
    let revisit = if matches!(run_state.as_str(), "live" | "stopping") {
        db.query_row(
            "SELECT value FROM app_meta WHERE key=?1",
            [format!("revisit.{run_id}")],
            |row| row.get(0),
        )
        .optional()?
    } else {
        None
    };
    let mut raw = RawProjection {
        run: None,
        turn: None,
        attempt: None,
        revisit,
        context_reduction: None,
        recovery_counts: None,
        owned,
        tool_running: false,
        tools: 0,
        cancelled: 0,
        live_work: if owned && matches!(run_state.as_str(), "live" | "stopping") {
            Some(super::live_work::read_tx(db, run_id, waits)?)
        } else {
            None
        },
    };
    if let Some(saved_turn) = turn {
        let turn_id = &saved_turn.id;
        let generation = saved_turn.generation;
        let reason = &saved_turn.recovery_reason;
        raw.context_reduction = reduction::read_tx(db, turn_id, generation)?;
        let attempt = db
            .query_row(
                "SELECT id,state,generation,CASE WHEN ?2 AND json_valid(error_json) THEN json_extract(error_json,'$.error') ELSE NULL END,CASE WHEN json_valid(error_json) THEN json_extract(error_json,'$.automatic_retry') ELSE NULL END,CASE WHEN json_valid(error_json) THEN CASE WHEN json_type(error_json,'$.automatic_retry_exhausted')='object' THEN json_extract(error_json,'$.automatic_retry_exhausted') ELSE NULL END ELSE NULL END FROM model_attempt WHERE turn_id=?1 ORDER BY rowid DESC LIMIT 1",
                rusqlite::params![turn_id,reason.is_some()],
                |r| Ok(SavedAttempt { id:r.get(0)?,state:r.get(1)?,generation:r.get(2)?,error:r.get(3)?,automatic_retry:r.get(4)?,automatic_retry_exhausted:r.get(5)? }),
            )
            .optional()?;
        raw.attempt = attempt;
        if reason.is_some() {
            let counts = db.query_row(
                "SELECT
                  (SELECT COUNT(*) FROM receipt WHERE target_thread_id=?1 AND state IN ('accepted','held')),
                  (SELECT COUNT(*) FROM tool_result r JOIN tool_group g ON g.id=r.group_id WHERE g.turn_id=?2),
                  (SELECT COUNT(*) FROM shell_execution WHERE owner_run_id=?3 AND state IN ('starting','running','stopping')),
                  (SELECT COUNT(*) FROM shell_execution WHERE owner_run_id=?3 AND state IN ('exited','failed','stopped') AND output_complete=0)",
                rusqlite::params![thread,turn_id,run_id],
                |row| Ok((row.get::<_,i64>(0)? as u64,row.get::<_,i64>(1)? as u64,row.get::<_,i64>(2)? as u64,row.get::<_,i64>(3)? as u64)),
            )?;
            raw.recovery_counts = Some(counts);
        }
        let tool_running = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_group WHERE turn_id=?1 AND state='running')",
            [turn_id],
            |r| r.get(0),
        )?;
        let tools=db.query_row("SELECT COUNT(*) FROM shell_execution WHERE turn_id=?1 AND json_extract(shell_json,'$.spawned')=1",[turn_id],|r|r.get::<_,i64>(0).map(|n|n as u64))?;
        let cancelled=db.query_row("SELECT COUNT(*) FROM tool_result tr JOIN tool_group g ON g.id=tr.group_id WHERE g.turn_id=?1 AND tr.status='cancelled'",[turn_id],|r|r.get::<_,i64>(0).map(|n|n as u64))?;
        raw.tool_running = tool_running;
        raw.tools = tools;
        raw.cancelled = cancelled;
        raw.turn = Some(saved_turn);
    }
    raw.run = run;
    Ok(raw)
}

pub(super) fn prepare(raw: RawProjection) -> Projection {
    let RawProjection {
        run,
        turn,
        attempt,
        revisit,
        context_reduction,
        recovery_counts,
        owned,
        tool_running,
        tools,
        cancelled,
        live_work,
    } = raw;
    let Some((run_id, run_state, stop_reason)) = run else {
        return Projection {
            snapshot: ActivitySnapshot::default(),
            observed_at: std::time::Instant::now(),
            tool_calls: 0,
            cancelled_calls: 0,
            poll_running: false,
        };
    };
    let revisit = revisit
        .as_deref()
        .and_then(crate::control::revisit::summary_from_body)
        .as_ref()
        .and_then(crate::presentation::revisit_label);
    let mut snapshot = ActivitySnapshot {
        live_work: live_work.map(super::live_work::prepare),
        revisit,
        run_id: Some(run_id),
        ..Default::default()
    };
    let mut state = None;
    let mut attempt_state = None;
    if let Some(SavedTurn {
        id: turn_id,
        state: turn_state,
        generation,
        recovery_reason: reason,
    }) = turn
    {
        snapshot.context_reduction = reduction::prepare(context_reduction);
        if let Some(reason) = reason
            && (turn_state == "recovery_wait" || serde_json::from_str::<ApiError>(&reason).is_ok())
        {
            // Startup interruption writes a different saved reason schema. Its
            // presence alone is not evidence that this Turn awaited recovery.
            let error = serde_json::from_str::<ApiError>(&reason).ok();
            let mut message = error
                .as_ref()
                .map(crate::config_setup::tui_error)
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
            if let Some(details) = error.as_ref().and_then(|e| e.details.as_ref()) {
                let facts = &details["recovery"];
                if facts.is_object() {
                    stage.push_str(&format!(
                        " · retry {}/{} · request {}B · tool {}B",
                        details["automatic_retries"],
                        details["retry_limit"],
                        facts["protected_request"]["bytes"],
                        facts["protected_latest_tool"]["bytes"]
                    ));
                    if let Some(actions) = facts["actions"].as_array() {
                        message.push_str(" · ");
                        message.push_str(
                            &actions
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(" "),
                        );
                    }
                }
            }
            let (pending_messages, saved_tool_results, live_shells, uncollected_shells) =
                recovery_counts.expect("saved recovery reason has collected recovery counts");
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
        snapshot.turn_in_progress = matches!(run_state.as_str(), "live" | "stopping")
            && matches!(turn_state.as_str(), "preparing" | "running" | "waiting");
        snapshot.turn_id = Some(turn_id);
        state = Some(turn_state);
    }
    snapshot.phase = crate::activity::classify(
        Some(&run_state),
        state.as_deref(),
        owned,
        attempt_state.as_deref() == Some("inflight"),
        snapshot.automatic_retry.is_some(),
        tool_running,
    );
    let poll_running = run_state == "stopping"
        || (run_state == "live"
            && matches!(
                state.as_deref(),
                Some("preparing" | "running" | "waiting" | "recovery_wait")
            ));
    Projection {
        snapshot,
        observed_at: std::time::Instant::now(),
        tool_calls: tools,
        cancelled_calls: cancelled,
        poll_running,
    }
}

/// Synchronous adapter for direct-connection callers and tests.
pub fn project(db: &Connection, thread: &str) -> Result<Projection, crate::storage::StorageError> {
    read_tx(db, thread).map(prepare)
}
pub async fn read(control: &Control, thread: &str) -> Result<Projection, ApiError> {
    let target = thread.to_owned();
    let waits = control.pending_wait_sources();
    let result = control
        .store
        .database
        .call(move |db| read_tx_with_waits(db, &target, waits.as_deref()))
        .await
        .map_err(super::storage_error);
    let raw = super::checked_storage(control, result).await?;
    super::presentation_work(move || prepare(raw)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owned_raw_snapshot_projects_saved_failure_and_revisit_after_connection_closes() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES('u','t','r','recovery_wait',2,'now');").unwrap();
        let reason = serde_json::to_string(&ApiError::new(
            crate::protocol::ErrorCode::IoFailed,
            "provider: saved reason",
        ))
        .unwrap();
        db.execute(
            "UPDATE turn SET recovery_reason_json=?1 WHERE id='u'",
            [reason],
        )
        .unwrap();
        let error = json!({"error":{"kind":"provider","message":"saved reason","status":500},
            "automatic_trim":{"before_characters":90000,"after_characters":40000,"omitted_nodes":7,"compact":{"spilled_groups":3,"no_target":false}}}).to_string();
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('a','u',2,0,'p','m',0,'incomplete','{}',?1,'now')",[error]).unwrap();
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('revisit.r',?1)",
            [json!({"pending":null,"consumed_count":3,"last_fired_at":"earlier"}).to_string()],
        )
        .unwrap();
        let raw = read_tx(&db, "t").unwrap();
        db.execute("UPDATE run SET state='completed' WHERE id='r'", [])
            .unwrap();
        db.execute("DELETE FROM model_attempt", []).unwrap();
        drop(db);
        let projection = prepare(raw);
        assert_eq!(projection.snapshot.run_id.as_deref(), Some("r"));
        assert_eq!(projection.snapshot.attempt_id.as_deref(), Some("a"));
        assert_eq!(projection.snapshot.revisit.as_deref(), Some("Revisit 3"));
        assert_eq!(
            projection.snapshot.context_reduction.as_deref(),
            Some("自動縮小済み · spill 3組 · 省略 7件 · 90000→40000 chars")
        );
        let recovery = projection.snapshot.recovery.unwrap();
        assert_eq!(recovery.message, "saved reason");
        assert_eq!(recovery.stage, "モデル要求");
        assert_eq!(recovery.live_shells, 0);
        assert!(
            projection.poll_running,
            "saved live recovery policy remains unchanged"
        );
    }

    #[test]
    fn background_only_uses_saved_live_work_without_periodic_turn_polling() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2020-01-01T00:00:00Z');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2020-01-01T00:00:00Z');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('s','r','u','op','running','/','{}','UTF-8','UTF-8','open','2020-01-01T00:00:00Z');").unwrap();
        let projection = prepare(read_tx_with_waits(&db, "t", Some(&[])).unwrap());
        assert!(!projection.poll_running);
        assert!(!projection.snapshot.turn_in_progress);
        assert_eq!(projection.snapshot.live_work.unwrap().shells, 1);
        db.execute("UPDATE run SET state='completed' WHERE id='r'", [])
            .unwrap();
        assert!(
            prepare(read_tx(&db, "t").unwrap())
                .snapshot
                .live_work
                .is_none(),
            "terminal Run must not imply live work from inconsistent retained rows"
        );
        db.execute("UPDATE run SET state='live' WHERE id='r'", [])
            .unwrap();
        db.execute("UPDATE shell_execution SET state='exited'", [])
            .unwrap();
        // A source table used exclusively by live-work observations may be
        // unavailable here: owned=false must not even prepare that query.
        db.execute("DROP TABLE output_chunk", []).unwrap();
        assert!(
            prepare(read_tx(&db, "t").unwrap())
                .snapshot
                .live_work
                .is_none()
        );
    }
}
