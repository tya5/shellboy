//! One read projection for the existing progress cadence; no new core state.
mod reduction;
use crate::{
    control::Control,
    protocol::ApiError,
    ui::{
        UiUpdate,
        activity::{
            ActivitySnapshot, AutomaticRetrySnapshot, RecoverySnapshot, RecoveryStage,
            RecoveryStageDetails,
        },
    },
};
use rusqlite::{Connection, OptionalExtension};

#[derive(Clone, Debug)]
pub struct Projection {
    pub snapshot: ActivitySnapshot,
    pub observed_at: std::time::Instant,
    pub tool_calls: u64,
    pub cancelled_calls: u64,
    pub counts_capped: bool,
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
    started_at: Option<String>,
    id: String,
    state: String,
    generation: i64,
    error: Option<String>,
    automatic_retry: Option<String>,
    automatic_retry_exhausted: Option<String>,
    attachment_unavailable: bool,
    attachment_unsupported: bool,
    attachment_check_limited: bool,
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
pub(super) fn saved_execution_recovery(error: &ApiError) -> bool {
    !error.is_storage_failure()
        && (error.code == crate::protocol::ErrorCode::ConfigInvalid
            || error
                .details
                .as_ref()
                .is_some_and(|d| d.get("recovery").is_some_and(serde_json::Value::is_object)))
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
            counts_capped: self.counts_capped,
            state: self.label().into(),
        }
    }
}
/// Owned SQL facts from one caller-controlled read transaction. Projection and
/// model-error/diagnostic decoding are deliberately outside the DB worker.
pub(super) struct RawProjection {
    run: Option<(String, String, Option<String>, Option<String>)>,
    turn: Option<SavedTurn>,
    attempt: Option<SavedAttempt>,
    revisit: Option<crate::protocol::RevisitSummary>,
    context_reduction: Option<String>,
    recovery_counts: Option<(u64, u64, u64, u64)>,
    counts_capped: bool,
    owned: bool,
    tool_running: bool,
    tool_started_at: Option<String>,
    tools: u64,
    cancelled: u64,
    live_work: Option<super::live_work::RawLiveWork>,
    waiting_reason: Option<crate::protocol::RunWaitReason>,
    automatic_compaction: Option<crate::ui::activity::AutomaticCompactionSnapshot>,
    automatic_compaction_skip: Option<crate::ui::activity::AutomaticCompactionSkip>,
}

#[derive(serde::Deserialize)]
struct AutomaticCompactionSkipWitness {
    run_id: String,
    turn_id: String,
    reason: crate::ui::activity::AutomaticCompactionSkip,
}

#[derive(serde::Deserialize)]
struct AutomaticCompactionWitness {
    run_id: String,
    turn_id: String,
    input_tokens: u64,
    threshold: u64,
    started_at: String,
}

// Both the group fanout and the number of result rows are bounded. A partial
// count stays a lower bound even when the first groups contain no results.
fn bounded_group_counts(
    db: &Connection,
    turn_id: &str,
) -> Result<(u64, u64, bool), crate::storage::StorageError> {
    let (saved, cancelled, groups): (i64, i64, i64) = db.query_row(
        "WITH groups AS MATERIALIZED (SELECT id FROM tool_group INDEXED BY activity_group_turn_state WHERE turn_id=?1 LIMIT 128),
         results AS MATERIALIZED (SELECT r.status FROM groups g CROSS JOIN tool_result r INDEXED BY sqlite_autoindex_tool_result_1 ON r.group_id=g.id LIMIT 128)
         SELECT (SELECT count(*) FROM results),
                (SELECT count(*) FROM results WHERE status='cancelled'),
                (SELECT count(*) FROM groups)",
        [turn_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok((
        saved as u64,
        cancelled as u64,
        groups == 128 || saved == 128 || cancelled == 128,
    ))
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
    read_tx_with_limit(db, thread, waits, 3)
}

pub(super) fn read_tx_with_limit(
    db: &Connection,
    thread: &str,
    waits: Option<&[(String, String)]>,
    limit: usize,
) -> Result<RawProjection, crate::storage::StorageError> {
    let run: Option<(String, String, Option<String>,Option<String>)> = db
        .query_row(
            "SELECT id,state,stop_reason,substr(ended_at,1,128) FROM run WHERE thread_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",
            [thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?,r.get(3)?)),
        )
        .optional()?;
    let Some((ref run_id, ref run_state, _, _)) = run else {
        return Ok(RawProjection {
            run,
            turn: None,
            attempt: None,
            revisit: None,
            context_reduction: None,
            recovery_counts: None,
            counts_capped: false,
            owned: false,
            tool_running: false,
            tool_started_at: None,
            tools: 0,
            cancelled: 0,
            live_work: None,
            waiting_reason: None,
            automatic_compaction: None,
            automatic_compaction_skip: None,
        });
    };
    let turn = db.query_row("SELECT id,state,generation,recovery_reason_json FROM turn INDEXED BY activity_turn_run WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[run_id],|r|Ok(SavedTurn { id:r.get(0)?,state:r.get(1)?,generation:r.get(2)?,recovery_reason:r.get(3)? })).optional()?;
    let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')) OR EXISTS(SELECT 1 FROM run WHERE owner_run_id=?1 AND state IN ('live','stopping'))",[run_id],|r|r.get(0))?;
    let revisit = if matches!(run_state.as_str(), "live" | "stopping") {
        crate::control::revisit::summary_for_run(db, run_id)?
    } else {
        None
    };
    let mut raw = RawProjection {
        run: None,
        waiting_reason: crate::readback::run_wait_reason(db, thread, run_id)
            .map_err(crate::storage::StorageError::from)?,
        automatic_compaction: None,
        automatic_compaction_skip: None,
        turn: None,
        attempt: None,
        revisit,
        context_reduction: None,
        recovery_counts: None,
        counts_capped: false,
        owned,
        tool_running: false,
        tool_started_at: None,
        tools: 0,
        cancelled: 0,
        live_work: if owned && matches!(run_state.as_str(), "live" | "stopping") {
            Some(super::live_work::read_tx_limited(db, run_id, waits, limit)?)
        } else {
            None
        },
    };
    if let Some(saved_turn) = turn {
        if run_state == "live" && matches!(saved_turn.state.as_str(), "preparing" | "running") {
            let body: Option<String> = db
                .query_row(
                    "SELECT substr(value,1,4096) FROM app_meta WHERE key=?1",
                    [crate::compaction::CompactionKeys::new(thread).automatic_active()],
                    |row| row.get(0),
                )
                .optional()?;
            let status: Option<String> = db
                .query_row(
                    "SELECT substr(value,1,4096) FROM app_meta WHERE key=?1",
                    [crate::compaction::CompactionKeys::new(thread).automatic_status()],
                    |row| row.get(0),
                )
                .optional()?;
            raw.automatic_compaction_skip = status
                .and_then(|body| serde_json::from_str::<AutomaticCompactionSkipWitness>(&body).ok())
                .filter(|saved| saved.run_id == *run_id && saved.turn_id == saved_turn.id)
                .map(|saved| saved.reason);
            raw.automatic_compaction = body
                .and_then(|body| serde_json::from_str::<AutomaticCompactionWitness>(&body).ok())
                .filter(|w| {
                    w.run_id == *run_id
                        && w.turn_id == saved_turn.id
                        && w.threshold > 0
                        && w.input_tokens >= w.threshold
                })
                .map(|w| crate::ui::activity::AutomaticCompactionSnapshot {
                    input_tokens: w.input_tokens,
                    threshold: w.threshold,
                    started_at: w.started_at,
                });
        }
        let turn_id = &saved_turn.id;
        let generation = saved_turn.generation;
        let reason = &saved_turn.recovery_reason;
        raw.context_reduction = reduction::read_tx(db, turn_id, generation)?;
        let attempt = db
            .query_row(
                "SELECT id,state,generation,CASE WHEN ?2 AND json_valid(error_json) THEN json_extract(error_json,'$.error') ELSE NULL END,CASE WHEN json_valid(error_json) THEN json_extract(error_json,'$.automatic_retry') ELSE NULL END,CASE WHEN json_valid(error_json) THEN CASE WHEN json_type(error_json,'$.automatic_retry_exhausted')='object' THEN json_extract(error_json,'$.automatic_retry_exhausted') ELSE NULL END ELSE NULL END,substr(started_at,1,128) FROM model_attempt INDEXED BY activity_attempt_turn WHERE turn_id=?1 ORDER BY rowid DESC LIMIT 1",
                rusqlite::params![turn_id,reason.is_some()],
                |r| Ok(SavedAttempt { started_at:r.get(6)?,id:r.get(0)?,state:r.get(1)?,generation:r.get(2)?,error:r.get(3)?,automatic_retry:r.get(4)?,automatic_retry_exhausted:r.get(5)?,attachment_unavailable:false,attachment_unsupported:false,attachment_check_limited:false }),
            )
            .optional()?;
        let mut attempt = attempt;
        if let Some(saved) = &mut attempt {
            // Bound the physical attempt prefix and retained error bodies before decoding.
            (saved.attachment_unavailable, saved.attachment_unsupported, saved.attachment_check_limited) = db.query_row(
                "WITH cohort AS MATERIALIZED (SELECT state,CASE WHEN octet_length(error_json)<=1024 THEN error_json END AS error FROM attachment_send WHERE attempt_id=?1 ORDER BY ordinal LIMIT 129) SELECT EXISTS(SELECT 1 FROM cohort WHERE state='unavailable'), EXISTS(SELECT 1 FROM cohort WHERE state='unavailable' AND CASE WHEN json_valid(error) THEN json_extract(error,'$.message')=?2 ELSE 0 END), (SELECT COUNT(*)=129 FROM cohort)",
                rusqlite::params![saved.id, crate::attachments::UNSUPPORTED_FORMAT_ERROR],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        }
        raw.attempt = attempt;
        let (saved_results, cancelled, groups_capped) = bounded_group_counts(db, turn_id)?;
        if reason.is_some() {
            let counts = db.query_row(
                "SELECT
                  (SELECT COUNT(*) FROM (SELECT 1 FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state IN ('accepted','held') LIMIT 128)),
                  (SELECT COUNT(*) FROM (SELECT 1 FROM shell_execution INDEXED BY shell_active_owner WHERE owner_run_id=?2 AND state IN ('starting','running','stopping') LIMIT 128)),
                  (SELECT COUNT(*) FROM (SELECT 1 FROM shell_execution INDEXED BY activity_shell_uncollected_owner WHERE owner_run_id=?2 AND state IN ('exited','failed','stopped') AND output_complete=0 LIMIT 128))",
                rusqlite::params![thread,run_id],
                |row| Ok((row.get::<_,i64>(0)? as u64,saved_results,row.get::<_,i64>(1)? as u64,row.get::<_,i64>(2)? as u64)),
            )?;
            raw.counts_capped |=
                groups_capped || [counts.0, counts.1, counts.2, counts.3].contains(&128);
            raw.recovery_counts = Some(counts);
        }
        let tool_started_at: Option<String> = db.query_row(
            "SELECT substr(created_at,1,128) FROM tool_group INDEXED BY activity_group_turn_state WHERE turn_id=?1 AND state='running' ORDER BY rowid LIMIT 1",
            [turn_id],
            |r| r.get(0),
        ).optional()?;
        let (tools, seen) = db.query_row("SELECT COUNT(*) FILTER (WHERE valid),COUNT(*) FROM (SELECT json_valid(shell_json) AS valid FROM shell_execution INDEXED BY activity_shell_spawned_turn WHERE turn_id=?1 AND CASE WHEN json_valid(shell_json) THEN json_extract(shell_json,'$.spawned')=1 ELSE 1 END LIMIT 128)",[turn_id],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64)))?;
        raw.counts_capped |= groups_capped || seen == 128 || tools < seen || cancelled == 128;
        raw.tool_running = tool_started_at.is_some();
        raw.tool_started_at = tool_started_at;
        raw.tools = tools;
        raw.cancelled = cancelled;
        raw.turn = Some(saved_turn);
    }
    raw.run = run;
    Ok(raw)
}

pub(super) fn prepare(raw: RawProjection) -> Projection {
    prepare_in(raw, crate::config::UiLanguage::Ja)
}

pub(super) fn prepare_in(raw: RawProjection, language: crate::config::UiLanguage) -> Projection {
    let RawProjection {
        run,
        turn,
        attempt,
        revisit,
        context_reduction,
        recovery_counts,
        counts_capped,
        owned,
        tool_running,
        tool_started_at,
        tools,
        cancelled,
        live_work,
        waiting_reason,
        automatic_compaction,
        automatic_compaction_skip,
    } = raw;
    let Some((run_id, run_state, stop_reason, ended_at)) = run else {
        return Projection {
            snapshot: ActivitySnapshot::default(),
            observed_at: std::time::Instant::now(),
            tool_calls: 0,
            cancelled_calls: 0,
            counts_capped: false,
            poll_running: false,
        };
    };
    let revisit = revisit
        .as_ref()
        .and_then(crate::presentation::revisit_label);
    let mut snapshot = ActivitySnapshot {
        tool_running,
        timing: crate::ui::activity::SavedTiming {
            tool_started_at,
            wall_at: Some(chrono::Utc::now()),
            ended_at: ended_at
                .as_deref()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok()),
            local_offset_seconds: chrono::Local::now().offset().local_minus_utc(),
            ..Default::default()
        },
        live_work: live_work.map(|raw| super::live_work::prepare_in(raw, language)),
        waiting_reason,
        automatic_compaction,
        automatic_compaction_skip,
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
        snapshot.timing.generation = Some(generation);
        snapshot.context_reduction = reduction::prepare(context_reduction);
        if let Some(reason) = reason
            && (turn_state == "recovery_wait" || serde_json::from_str::<ApiError>(&reason).is_ok())
        {
            // Startup interruption writes a different saved reason schema. Its
            // presence alone is not evidence that this Turn awaited recovery.
            let error = serde_json::from_str::<ApiError>(&reason).ok();
            let message_is_fallback = error.is_none();
            let mut message = error
                .as_ref()
                .map(crate::config_setup::tui_error)
                .unwrap_or_else(|| {
                    crate::ui::activity::saved_recovery_reason_unavailable(
                        crate::config::UiLanguage::Ja,
                    )
                    .into()
                });
            let mut stage = RecoveryStage::Unrecorded;
            let mut stage_details = None;
            let mut stage_detail_values = None;
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
                    RecoveryStage::ModelContextLimit
                } else {
                    RecoveryStage::ModelRequest
                };
            }
            if let Some(details) = error.as_ref().and_then(|e| e.details.as_ref()) {
                let facts = &details["recovery"];
                if facts.is_object() {
                    let values = (
                        details["automatic_retries"].clone(),
                        details["retry_limit"].clone(),
                        facts["protected_request"]["bytes"].clone(),
                        facts["protected_latest_tool"]["bytes"].clone(),
                    );
                    stage_detail_values = Some(values.clone());
                    stage_details = Some(RecoveryStageDetails {
                        retries: values.0,
                        retry_limit: values.1,
                        request_bytes: values.2,
                        tool_bytes: values.3,
                    });
                    if let Some(actions) = facts["actions"].as_array() {
                        message.push_str(crate::ui::text_width::glyphs::separator());
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
                resolved: !(run_state == "failed"
                    || (matches!(run_state.as_str(), "live" | "stopping")
                        && matches!(turn_state.as_str(), "recovery_wait" | "failed"))),
                usage_limit: error
                    .as_ref()
                    .and_then(|e| e.details.as_ref())
                    .and_then(|d| d.get("usage_limit"))
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
                content_filter: error
                    .as_ref()
                    .and_then(|e| e.details.as_ref())
                    .and_then(|d| d.get("content_filter"))
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
                stage: if let Some((retries, retry_limit, request_bytes, tool_bytes)) =
                    stage_detail_values
                {
                    let separator = crate::ui::text_width::glyphs::separator();
                    format!(
                        "{}{separator}retry {retries}/{retry_limit}{separator}request {request_bytes}B{separator}tool {tool_bytes}B",
                        stage.localized(crate::config::UiLanguage::Ja)
                    )
                } else {
                    stage.localized(crate::config::UiLanguage::Ja).into()
                },
                stage_kind: Some(stage),
                stage_details,
                message: if message_is_fallback {
                    crate::ui::activity::saved_recovery_reason_unavailable(
                        crate::config::UiLanguage::Ja,
                    )
                    .into()
                } else {
                    message
                },
                message_is_fallback,
                pending_messages,
                saved_tool_results,
                live_shells,
                uncollected_shells,
                counts_capped,
                stop_reason: stop_reason.clone(),
            });
        }
        if let Some(SavedAttempt {
            id,
            started_at,
            state: saved_state,
            generation: attempt_generation,
            automatic_retry,
            attachment_unavailable,
            attachment_unsupported,
            attachment_check_limited,
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
            snapshot.attachment_unavailable =
                attachment_unavailable && attempt_generation == generation;
            snapshot.attachment_unsupported =
                attachment_unsupported && attempt_generation == generation;
            snapshot.attachment_check_limited =
                attachment_check_limited && attempt_generation == generation;
            snapshot.timing.attempt_started_at = started_at;
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
        counts_capped,
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
    let limit = control.store.config.ui.live_work_rows;
    let result = control
        .store
        .database
        .call(move |db| read_tx_with_limit(db, &target, waits.as_deref(), limit))
        .await
        .map_err(super::storage_error);
    let raw = super::checked_storage(control, result).await?;
    let language = control.store.config.ui.language;
    super::presentation_work(move || prepare_in(raw, language)).await
}

pub(super) async fn read_cancellable(
    control: &Control,
    thread: &str,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Projection, ApiError> {
    let target = thread.to_owned();
    let waits = control.pending_wait_sources();
    let limit = control.store.config.ui.live_work_rows;
    let path = control.store.database.path().to_path_buf();
    let index = control.index.clone();
    let result = super::snapshot::read(path, stop, move |db| {
        let raw = read_tx_with_limit(db, &target, waits.as_deref(), limit)?;
        let pause = index.read_display_pause(db)?;
        Ok((raw, pause))
    })
    .await;
    let (raw, pause) = super::checked_storage(control, result).await?;
    let language = control.store.config.ui.language;
    super::presentation_work(move || {
        let mut projection = prepare_in(raw, language);
        projection.snapshot.index_pause = pause;
        projection
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_latest_and_running_state_are_index_seeks() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        for (sql, index) in [
            (
                "SELECT id FROM turn INDEXED BY activity_turn_run WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",
                "activity_turn_run",
            ),
            (
                "SELECT rowid FROM model_attempt INDEXED BY activity_attempt_turn WHERE turn_id=?1 ORDER BY rowid DESC LIMIT 1",
                "activity_attempt_turn",
            ),
            (
                "SELECT created_at FROM tool_group INDEXED BY activity_group_turn_state WHERE turn_id=?1 AND state='running' ORDER BY rowid LIMIT 1",
                "activity_group_turn_state",
            ),
            (
                "SELECT 1 FROM turn WHERE run_id=?1 AND state IN ('preparing','running') LIMIT 1",
                "activity_turn_run_state",
            ),
        ] {
            let mut plan = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let detail: Vec<String> = plan
                .query_map(["id"], |r| r.get(3))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert!(detail.iter().any(|line| line.contains(index)), "{detail:?}");
            assert!(
                !detail.iter().any(|line| line.contains("TEMP B-TREE")),
                "{detail:?}"
            );
        }
    }

    #[test]
    fn result_counts_are_bounded_even_when_groups_or_results_are_sparse() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('a','u',0,0,'p','m',0,'completed','{}','now'); INSERT INTO node(id,kind,created_at,payload_json) VALUES('n','user','now','{}');").unwrap();
        for i in 0..127 {
            db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES(?1,'a','u','done','now')", [format!("g{i:03}")]).unwrap();
        }
        assert_eq!(bounded_group_counts(&db, "u").unwrap(), (0, 0, false));
        db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES('g127','a','u','done','now')", []).unwrap();
        db.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES('g128','a','u','done','now')", []).unwrap();
        assert_eq!(bounded_group_counts(&db, "u").unwrap(), (0, 0, true));
        for i in 0..129 {
            db.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state) VALUES('g000',?1,?2,'test','{}','completed')", rusqlite::params![format!("c{i:03}"), i]).unwrap();
            db.execute(
                "INSERT INTO tool_result(group_id,call_id,node_id,status) VALUES('g000',?1,'n',?2)",
                rusqlite::params![
                    format!("c{i:03}"),
                    if i == 0 { "cancelled" } else { "completed" }
                ],
            )
            .unwrap();
        }
        assert_eq!(bounded_group_counts(&db, "u").unwrap(), (128, 1, true));
    }

    #[test]
    fn malformed_shell_metadata_makes_tool_count_a_lower_bound() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now'); PRAGMA ignore_check_constraints=ON; INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('bad','r','u','bad','exited','/','broken json','UTF-8','UTF-8','closed','now'),('good','r','u','good','exited','/','{\"spawned\":true}','UTF-8','UTF-8','closed','now'); PRAGMA ignore_check_constraints=OFF;").unwrap();
        let projection = prepare(read_tx(&db, "t").unwrap());
        assert_eq!(projection.tool_calls, 1);
        assert!(projection.counts_capped);
    }

    #[test]
    fn attachment_warning_reads_only_the_latest_attempt_and_distinguishes_format() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('n','user','now','{}'); INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('file','n','/synthetic','file','external','now'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','now');").unwrap();
        db.execute("INSERT INTO attachment_send(attempt_id,ordinal,attachment_id,state,error_json)VALUES('a',0,'file','unavailable',?1)", [json!({"message":crate::attachments::UNSUPPORTED_FORMAT_ERROR}).to_string()]).unwrap();
        let snapshot = prepare(read_tx(&db, "t").unwrap()).snapshot;
        assert!(snapshot.attachment_unavailable);
        assert!(snapshot.attachment_unsupported);
        db.execute(
            "UPDATE attachment_send SET error_json=?1",
            [json!({"message":"file missing"}).to_string()],
        )
        .unwrap();
        let snapshot = prepare(read_tx(&db, "t").unwrap()).snapshot;
        assert!(snapshot.attachment_unavailable);
        assert!(!snapshot.attachment_unsupported);
        db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('b','u',0,0,'p','m',0,'completed','{}','later')", []).unwrap();
        let snapshot = prepare(read_tx(&db, "t").unwrap()).snapshot;
        assert!(!snapshot.attachment_unavailable);
        assert!(!snapshot.attachment_unsupported);
        assert!(!snapshot.attachment_check_limited);
        for ordinal in 0..129 {
            db.execute("INSERT INTO attachment_send(attempt_id,ordinal,attachment_id,state) VALUES('b',?1,'file','ready')", [ordinal]).unwrap();
        }
        let snapshot = prepare(read_tx(&db, "t").unwrap()).snapshot;
        assert!(snapshot.attachment_check_limited);
        assert!(!snapshot.attachment_unavailable);
        db.execute("UPDATE turn SET generation=1 WHERE id='u'", [])
            .unwrap();
        let snapshot = prepare(read_tx(&db, "t").unwrap()).snapshot;
        assert!(
            !snapshot.attachment_check_limited,
            "stale attempts cannot create current warnings"
        );
    }

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
        crate::control::revisit::seed_fixture(&db, "t", "r", 3, false);
        let raw = read_tx(&db, "t").unwrap();
        db.execute("UPDATE run SET state='completed' WHERE id='r'", [])
            .unwrap();
        db.execute("DELETE FROM model_attempt", []).unwrap();
        drop(db);
        let projection = prepare(raw);
        assert_eq!(projection.snapshot.run_id.as_deref(), Some("r"));
        assert_eq!(projection.snapshot.attempt_id.as_deref(), Some("a"));
        assert_eq!(projection.snapshot.revisit.as_deref(), Some("↻3"));
        let reduction = projection.snapshot.context_reduction.unwrap();
        assert_eq!(
            reduction.localized(crate::config::UiLanguage::Ja),
            "自動縮小済み | spill 3組 | 省略 7件 | 90000>40000 chars"
        );
        assert!(
            reduction
                .localized(crate::config::UiLanguage::En)
                .is_ascii()
        );
        let recovery = projection.snapshot.recovery.unwrap();
        assert_eq!(recovery.message, "saved reason");
        assert_eq!(recovery.stage_kind, Some(RecoveryStage::ModelRequest));
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

#[cfg(test)]
mod footer85_tests {
    use super::*;
    #[test]
    fn saved_recovery_is_current_only_until_explicit_saved_resolution() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','recovery_wait','now');").unwrap();
        let limit = serde_json::json!({"kind":"usage_limit_reached","resets_at":1893499200_i64,"resets_at_utc":"2030-01-01T12:00:00Z","locally_blocked":true,"limit_window_minutes":10080});
        let reason = ApiError::new(
            crate::protocol::ErrorCode::IoFailed,
            "original provider failure",
        )
        .details(serde_json::json!({"usage_limit":limit}));
        db.execute(
            "UPDATE turn SET recovery_reason_json=?1",
            [serde_json::to_string(&reason).unwrap()],
        )
        .unwrap();
        let p = project(&db, "t").unwrap();
        let recovery = p.snapshot.recovery.unwrap();
        assert!(recovery.usage_limit.unwrap().locally_blocked);
        assert!(recovery.limit_summary().unwrap().contains("UTC"));
        for state in ["running", "completed", "stopped"] {
            db.execute("UPDATE turn SET state=?1", [state]).unwrap();
            assert!(
                project(&db, "t")
                    .unwrap()
                    .snapshot
                    .recovery
                    .as_ref()
                    .is_some_and(|r| r.resolved),
                "saved resolved state {state} wins over retained historical reason"
            );
        }
        db.execute("UPDATE turn SET state='failed'", []).unwrap();
        assert!(project(&db, "t").unwrap().snapshot.recovery.is_some());
        db.execute("UPDATE run SET state='stopped'", []).unwrap();
        assert!(
            project(&db, "t")
                .unwrap()
                .snapshot
                .recovery
                .unwrap()
                .resolved
        );
        let original: String = db
            .query_row("SELECT recovery_reason_json FROM turn", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ApiError>(&original).unwrap().message,
            reason.message
        );
    }
}

#[cfg(test)]
mod recovery_notice_tests {
    use super::*;
    #[test]
    fn only_saved_execution_errors_defer_to_reactive_projection() {
        use crate::protocol::ErrorCode;
        assert!(!saved_execution_recovery(&ApiError::new(
            ErrorCode::IoFailed,
            "unclassified"
        )));
        assert!(saved_execution_recovery(
            &ApiError::new(ErrorCode::IoFailed, "saved")
                .details(serde_json::json!({"recovery":{}}))
        ));
        assert!(!saved_execution_recovery(
            &ApiError::new(ErrorCode::SaveFailed, "storage")
                .details(serde_json::json!({"recovery":{}}))
        ));
        assert!(saved_execution_recovery(&ApiError::new(
            ErrorCode::ConfigInvalid,
            "configuration"
        )));
        assert!(!saved_execution_recovery(
            &ApiError::new(ErrorCode::IoFailed, "bad type")
                .details(serde_json::json!({"recovery":"untrusted text"}))
        ));
    }
}
