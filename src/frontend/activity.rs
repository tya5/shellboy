//! One activity projection from the owner's `thread.get` activity, the
//! running shells' waits and the index pause; no new core state.
use crate::{
    control::Control,
    protocol::{ApiError, ThreadActivity, ThreadActivityErrorKind},
    ui::{
        UiUpdate,
        activity::{
            ActivitySnapshot, AutomaticRetrySnapshot, RecoverySnapshot, RecoveryStage,
            RecoveryStageDetails,
        },
    },
};

#[derive(Clone, Debug)]
pub struct Projection {
    pub snapshot: ActivitySnapshot,
    pub observed_at: std::time::Instant,
    pub tool_calls: u64,
    pub cancelled_calls: u64,
    pub counts_capped: bool,
    pub poll_running: bool,
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

/// The wire name of a saved state, as the shared activity classifier reads it.
fn wire<T: serde::Serialize>(state: &T) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Whether the Run owns live work worth the owner's shell rows.
fn has_live_work(activity: &ThreadActivity) -> bool {
    use crate::protocol::RunState;
    activity.run.owned_shells > 0
        && matches!(activity.run.state, RunState::Live | RunState::Stopping)
}

/// The footer projection of what `thread.get` reports as `activity`, without
/// live work rows.
pub fn from_thread_activity(
    activity: Option<ThreadActivity>,
    language: crate::config::UiLanguage,
) -> Projection {
    prepare_in(activity, None, language)
}

#[cfg(test)]
pub(super) fn prepare(activity: Option<ThreadActivity>) -> Projection {
    from_thread_activity(activity, crate::config::UiLanguage::Ja)
}

pub(super) fn prepare_in(
    activity: Option<ThreadActivity>,
    live_work: Option<super::live_work::RawLiveWork>,
    language: crate::config::UiLanguage,
) -> Projection {
    let Some(ThreadActivity {
        run,
        turn,
        current_attempt: attempt,
        automatic_compaction,
    }) = activity
    else {
        return Projection {
            snapshot: ActivitySnapshot::default(),
            observed_at: std::time::Instant::now(),
            tool_calls: 0,
            cancelled_calls: 0,
            counts_capped: false,
            poll_running: false,
        };
    };
    let run_state = wire(&run.state);
    let revisit = run
        .revisit
        .as_ref()
        .and_then(crate::presentation::revisit_label);
    let tools = turn.as_ref().map(|turn| &turn.tools);
    let tool_started_at = tools.and_then(|tools| tools.running_since.clone());
    let tool_running = tool_started_at.is_some();
    let mut counts_capped = tools.is_some_and(|tools| tools.capped);
    let mut snapshot = ActivitySnapshot {
        tool_running,
        timing: crate::ui::activity::SavedTiming {
            tool_started_at,
            wall_at: Some(chrono::Utc::now()),
            ended_at: run
                .ended_at
                .as_deref()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok()),
            local_offset_seconds: chrono::Local::now().offset().local_minus_utc(),
            ..Default::default()
        },
        live_work: live_work.map(|raw| super::live_work::prepare_in(raw, language)),
        waiting_reason: run.waiting_reason,
        automatic_compaction: automatic_compaction.map(|witness| {
            crate::ui::activity::AutomaticCompactionSnapshot {
                input_tokens: witness.input_tokens,
                threshold: witness.threshold,
                started_at: witness.started_at,
            }
        }),
        revisit,
        run_id: Some(run.run_id),
        ..Default::default()
    };
    let mut state = None;
    let mut attempt_state = None;
    let (tool_calls, cancelled_calls) =
        tools.map_or((0, 0), |tools| (tools.spawned, tools.cancelled));
    if let Some(turn) = turn {
        let turn_state = wire(&turn.state);
        snapshot.timing.generation = Some(turn.generation);
        snapshot.context_reduction =
            turn.context_reduction
                .map(|reduction| crate::ui::activity::ContextReductionSummary {
                    before_characters: reduction.before_characters,
                    after_characters: reduction.after_characters,
                    spilled_groups: reduction.spilled_groups,
                    omitted_items: reduction.omitted_items,
                });
        if let Some(reason) = &turn.recovery_reason
            && (turn_state == "recovery_wait" || serde_json::from_str::<ApiError>(reason).is_ok())
        {
            // Startup interruption writes a different saved reason schema. Its
            // presence alone is not evidence that this Turn awaited recovery.
            let error = serde_json::from_str::<ApiError>(reason).ok();
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
            // The owner matched this reason to the attempt's own model error.
            if let Some(attempt) = &attempt
                && let (Some(kind), Some(projected)) = (attempt.error_kind, &attempt.error_message)
            {
                message = projected.clone();
                stage = match kind {
                    ThreadActivityErrorKind::ModelContextLimit => RecoveryStage::ModelContextLimit,
                    ThreadActivityErrorKind::ModelRequest => RecoveryStage::ModelRequest,
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
            if let Some(hint) = error.as_ref().and_then(|error| error.hint.as_deref()) {
                message.push_str(crate::ui::text_width::glyphs::separator());
                message.push_str(hint);
            }
            let counts = turn
                .recovery_counts
                .as_ref()
                .expect("saved recovery reason has collected recovery counts");
            counts_capped |= counts.capped;
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
                pending_messages: counts.receipts,
                saved_tool_results: counts.results,
                live_shells: counts.active_shells,
                uncollected_shells: counts.uncollected_shells,
                counts_capped,
                stop_reason: run.stop_reason.clone(),
            });
        }
        if let Some(attempt) = attempt {
            let saved_state = wire(&attempt.state);
            if run_state == "live" && turn_state == "running" && saved_state == "failed" {
                snapshot.automatic_retry =
                    attempt.automatic_retry.map(|retry| AutomaticRetrySnapshot {
                        number: retry.number,
                        limit: retry.limit,
                        delay_ms: retry.delay_ms,
                        partial_response_discarded: retry.partial_response_discarded,
                    });
            }
            snapshot.attachment_unavailable = attempt.attachment_unavailable;
            snapshot.attachment_unsupported = attempt.attachment_unsupported;
            snapshot.attachment_check_limited = attempt.attachment_check_limited;
            snapshot.timing.attempt_started_at = attempt.started_at;
            snapshot.attempt_id = Some(attempt.attempt_id);
            attempt_state = Some(saved_state);
        }
        snapshot.turn_in_progress = matches!(run_state.as_str(), "live" | "stopping")
            && matches!(turn_state.as_str(), "preparing" | "running" | "waiting");
        snapshot.turn_id = Some(turn.turn_id);
        state = Some(turn_state);
    }
    snapshot.phase = crate::activity::classify(
        Some(&run_state),
        state.as_deref(),
        run.owned_shells > 0,
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
        tool_calls,
        cancelled_calls,
        counts_capped,
        poll_running,
    }
}

/// One projection over the embedded owner's own listener.
pub async fn read(control: &Control, thread: &str) -> Result<Projection, ApiError> {
    let owner = super::native_history::Owner {
        data_dir: super::compose_data_dir(control),
        instance: &control.store.instance_id,
    };
    read_cancellable(
        control,
        &owner,
        thread,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
}

/// Read the selected header's activity, then project it.
pub(super) async fn read_cancellable(
    control: &Control,
    owner: &super::native_history::Owner<'_>,
    thread: &str,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Projection, ApiError> {
    let header = super::native_header(owner.data_dir, owner.instance, thread).await?;
    project_native(control, owner, thread, header.activity().cloned(), stop).await
}

/// Project an owner activity already read with its header, joining the Run's
/// running shells (`shell.list`) and the index pause (`index.status`).
pub(super) async fn project_native(
    control: &Control,
    owner: &super::native_history::Owner<'_>,
    thread: &str,
    activity: Option<ThreadActivity>,
    stop: tokio_util::sync::CancellationToken,
) -> Result<Projection, ApiError> {
    use crate::protocol::{Command, IndexStatus};
    let live_work = match &activity {
        Some(activity) if has_live_work(activity) => Some(
            super::live_work::read(
                owner,
                thread,
                &activity.run.run_id,
                activity.run.owned_shells,
                control.store.config.ui.live_work_rows,
                control.store.config.ui.language,
            )
            .await?,
        ),
        _ => None,
    };
    let index: IndexStatus = owner.call(Command::IndexStatus(Default::default())).await?;
    if stop.is_cancelled() {
        return Err(ApiError::new(
            crate::protocol::ErrorCode::InvalidState,
            "Activity observation closed",
        ));
    }
    let language = control.store.config.ui.language;
    super::presentation_work(move || {
        let mut projection = prepare_in(activity, live_work, language);
        projection.snapshot.index_pause = index.pause_kind;
        projection
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;

    fn read_tx(
        db: &Connection,
        thread: &str,
    ) -> Result<Option<ThreadActivity>, crate::storage::StorageError> {
        crate::control::db::thread_activity::read(db, thread)
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
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','p','m','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now'); INSERT INTO node(id,kind,created_at,payload_json)VALUES('n','user','now','{}'); INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES('file','n','/synthetic','file','external','now'); INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'p','m',0,'completed','{}','now');").unwrap();
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
        db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'b','u',0,0,'p','m',0,'completed','{}','later')", []).unwrap();
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
        let provider_error =
            json!({"error":{"kind":"provider","message":"saved reason","status":500}}).to_string();
        db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',2,0,'p','m',0,'incomplete','{}',?1,'now')",[provider_error]).unwrap();
        let reduction = json!({"metrics":{"before_characters":90000,"after_characters":40000,"omitted_nodes":7,"compact":{"spilled_groups":3,"no_target":false}}}).to_string();
        db.execute("INSERT INTO automatic_compaction(attempt_id,turn_id,attempt_started_at,outcome,result_json) VALUES('a','u','now','adopted',?1)", [reduction]).unwrap();
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
        let activity = read_tx(&db, "t").unwrap().unwrap();
        assert!(has_live_work(&activity));
        let projection = prepare(Some(activity));
        assert!(!projection.poll_running);
        assert!(!projection.snapshot.turn_in_progress);
        db.execute("UPDATE run SET state='completed' WHERE id='r'", [])
            .unwrap();
        assert!(
            !has_live_work(&read_tx(&db, "t").unwrap().unwrap()),
            "terminal Run must not imply live work from inconsistent retained rows"
        );
        db.execute("UPDATE run SET state='live' WHERE id='r'", [])
            .unwrap();
        db.execute("UPDATE shell_execution SET state='exited'", [])
            .unwrap();
        assert!(!has_live_work(&read_tx(&db, "t").unwrap().unwrap()));
    }
}

#[cfg(test)]
mod footer85_tests {
    use super::*;
    use rusqlite::Connection;
    /// What `thread.get` reports as `activity`, projected for the footer.
    fn saved(db: &Connection) -> Projection {
        prepare(crate::control::db::thread_activity::read(db, "t").unwrap())
    }
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
        let p = saved(&db);
        let recovery = p.snapshot.recovery.unwrap();
        assert!(recovery.usage_limit.unwrap().locally_blocked);
        assert!(recovery.limit_summary().unwrap().contains("UTC"));
        for state in ["running", "completed", "stopped"] {
            db.execute("UPDATE turn SET state=?1", [state]).unwrap();
            assert!(
                saved(&db)
                    .snapshot
                    .recovery
                    .as_ref()
                    .is_some_and(|r| r.resolved),
                "saved resolved state {state} wins over retained historical reason"
            );
        }
        db.execute("UPDATE turn SET state='failed'", []).unwrap();
        assert!(saved(&db).snapshot.recovery.is_some());
        db.execute("UPDATE run SET state='stopped'", []).unwrap();
        assert!(saved(&db).snapshot.recovery.unwrap().resolved);
        let original: String = db
            .query_row("SELECT recovery_reason_json FROM turn", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ApiError>(&original).unwrap().message,
            reason.message
        );
    }
}
