//! The selected Thread's current work as one bounded owner projection. The
//! history.window activity IDs and `thread.get` activity both come from here.
use crate::protocol::{
    ApiError, AttemptState, RunState, ThreadActivity, ThreadActivityAttempt,
    ThreadActivityCompaction, ThreadActivityErrorKind, ThreadActivityRecoveryCounts,
    ThreadActivityReduction, ThreadActivityRetry, ThreadActivityRetryExhausted, ThreadActivityRun,
    ThreadActivityTools, ThreadActivityTurn, TurnState,
};
use crate::storage::StorageError;
use rusqlite::{Connection, OptionalExtension};

type Result<T> = std::result::Result<T, StorageError>;
/// Latest attempt row: id, state, generation, error, retry, exhausted, started.
type SavedAttempt = (
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

// Search only the latest 256 attempts in this Turn. The indexed reverse walk
// never loads full result_json into Rust, and only integer facts leave SQLite.
// A missing result means no evidence in this bounded window, not "never trimmed".
const REDUCTION: &str = "WITH recent AS (
 SELECT generation,CASE WHEN generation=?2 THEN CASE WHEN json_valid(result_json) THEN
  CASE WHEN json_type(result_json,'$.metrics')='object'
   AND json_type(result_json,'$.metrics.before_characters')='integer'
   AND json_type(result_json,'$.metrics.after_characters')='integer'
   AND json_type(result_json,'$.metrics.compact.spilled_groups')='integer'
   AND json_type(result_json,'$.metrics.omitted_nodes')='integer'
   AND typeof(json_extract(result_json,'$.metrics.before_characters'))='integer'
   AND typeof(json_extract(result_json,'$.metrics.after_characters'))='integer'
   AND typeof(json_extract(result_json,'$.metrics.compact.spilled_groups'))='integer'
   AND typeof(json_extract(result_json,'$.metrics.omitted_nodes'))='integer'
   AND json_extract(result_json,'$.metrics.before_characters')>0
   AND json_extract(result_json,'$.metrics.after_characters')>=0
   AND json_extract(result_json,'$.metrics.after_characters')<json_extract(result_json,'$.metrics.before_characters')
   AND json_extract(result_json,'$.metrics.compact.spilled_groups')>=0
   AND json_extract(result_json,'$.metrics.omitted_nodes')>=0
   AND (json_extract(result_json,'$.metrics.compact.spilled_groups')>0 OR json_extract(result_json,'$.metrics.omitted_nodes')>0)
   AND json_extract(result_json,'$.metrics.compact.no_target') IS NOT 1
  THEN json_array(
   json_extract(result_json,'$.metrics.before_characters'),
   json_extract(result_json,'$.metrics.after_characters'),
   json_extract(result_json,'$.metrics.compact.spilled_groups'),
   json_extract(result_json,'$.metrics.omitted_nodes')) END END END AS facts
 FROM model_attempt a INDEXED BY attempts_by_turn LEFT JOIN automatic_compaction c ON c.attempt_id=a.id WHERE a.turn_id=?1
 ORDER BY a.started_at DESC,a.rowid DESC LIMIT 256
 ) SELECT facts FROM recent WHERE generation=?2 AND facts IS NOT NULL LIMIT 1";

fn decode<T: serde::de::DeserializeOwned>(value: &str, what: &str) -> Result<T> {
    serde_json::from_value(serde_json::json!(value)).map_err(|_| {
        StorageError::from(ApiError::new(
            crate::protocol::ErrorCode::InvalidState,
            format!("unknown saved {what} state"),
        ))
    })
}

// Both the group fanout and the number of result rows are bounded. A partial
// count stays a lower bound even when the first groups contain no results.
fn bounded_group_counts(db: &Connection, turn_id: &str) -> Result<(u64, u64, bool)> {
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

/// Match only complete diagnostics constructed from the typed saved facts.
/// Provider text and the durable attempt remain unchanged.
fn model_failure_message(
    message: &str,
    model: &crate::model::ModelError,
    exhausted: Option<&ThreadActivityRetryExhausted>,
) -> Option<String> {
    if message == model.to_string() {
        return Some(model.message.clone());
    }
    let counts = exhausted?;
    let suffix = format!(
        " (automatic retries exhausted: {}/{})",
        counts.retries, counts.limit
    );
    (message == format!("{model}{suffix}")).then(|| format!("{}{suffix}", model.message))
}

/// The latest Run of `thread`, its latest Turn and that Turn's latest attempt,
/// read in the caller's transaction. `None` when the Thread has no Run.
pub(crate) fn read(db: &Connection, thread: &str) -> Result<Option<ThreadActivity>> {
    let run: Option<(String, String, Option<String>, Option<String>)> = db
        .query_row(
            "SELECT id,state,stop_reason,substr(ended_at,1,128) FROM run WHERE thread_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",
            [thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((run_id, run_state, stop_reason, ended_at)) = run else {
        return Ok(None);
    };
    let owned_shells: i64 = db.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM shell_execution INDEXED BY shell_active_owner WHERE owner_run_id=?1 AND state IN ('starting','running','stopping') LIMIT 128)",
        [&run_id],
        |r| r.get(0),
    )?;
    let live = matches!(run_state.as_str(), "live" | "stopping");
    let revisit = if live {
        crate::control::revisit::summary_for_run(db, &run_id)?
    } else {
        None
    };
    let waiting_reason = crate::readback::run_wait_reason(db, thread, &run_id)?;
    let saved_turn: Option<(String, String, i64, Option<String>)> = db.query_row(
        "SELECT id,state,generation,recovery_reason_json FROM turn INDEXED BY activity_turn_run WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",
        [&run_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional()?;
    let mut activity = ThreadActivity {
        run: ThreadActivityRun {
            state: decode::<RunState>(&run_state, "Run")?,
            run_id: run_id.clone(),
            stop_reason,
            ended_at,
            owned_shells: owned_shells as u64,
            revisit,
            waiting_reason,
        },
        turn: None,
        current_attempt: None,
        automatic_compaction: None,
    };
    let Some((turn_id, turn_state, generation, reason)) = saved_turn else {
        return Ok(Some(activity));
    };
    let reason = crate::control::usage_limit_retry::public_reason(reason);
    activity.automatic_compaction = crate::compaction::read_automatic_activity(
        db,
        thread,
        &run_id,
        &run_state,
        &turn_id,
        &turn_state,
    )?
    .map(|witness| ThreadActivityCompaction {
        input_tokens: witness.input_tokens,
        threshold: witness.threshold,
        started_at: witness.started_at,
    });
    let context_reduction = db
        .query_row(REDUCTION, rusqlite::params![turn_id, generation], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
        .and_then(|body| serde_json::from_str::<[u64; 4]>(&body).ok())
        .map(
            |[
                before_characters,
                after_characters,
                spilled_groups,
                omitted_items,
            ]| {
                ThreadActivityReduction {
                    before_characters,
                    after_characters,
                    spilled_groups,
                    omitted_items,
                }
            },
        );
    let (results, cancelled, groups_capped) = bounded_group_counts(db, &turn_id)?;
    let recovery_counts = if reason.is_some() {
        let (receipts, active_shells, uncollected_shells): (i64, i64, i64) = db.query_row(
            "SELECT
              (SELECT COUNT(*) FROM (SELECT 1 FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state IN ('accepted','held') LIMIT 128)),
              (SELECT COUNT(*) FROM (SELECT 1 FROM shell_execution INDEXED BY shell_active_owner WHERE owner_run_id=?2 AND state IN ('starting','running','stopping') LIMIT 128)),
              (SELECT COUNT(*) FROM (SELECT 1 FROM shell_execution INDEXED BY activity_shell_uncollected_owner WHERE owner_run_id=?2 AND state IN ('exited','failed','stopped') AND output_complete=0 LIMIT 128))",
            rusqlite::params![thread, run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let counts = [
            receipts as u64,
            results,
            active_shells as u64,
            uncollected_shells as u64,
        ];
        Some(ThreadActivityRecoveryCounts {
            receipts: counts[0],
            results: counts[1],
            active_shells: counts[2],
            uncollected_shells: counts[3],
            capped: groups_capped || counts.contains(&128),
        })
    } else {
        None
    };
    let running_since: Option<String> = db.query_row(
        "SELECT substr(created_at,1,128) FROM tool_group INDEXED BY activity_group_turn_state WHERE turn_id=?1 AND state='running' ORDER BY rowid LIMIT 1",
        [&turn_id],
        |r| r.get(0),
    ).optional()?;
    let (spawned, seen): (i64, i64) = db.query_row("SELECT COUNT(*) FILTER (WHERE valid),COUNT(*) FROM (SELECT json_valid(shell_json) AS valid FROM shell_execution INDEXED BY activity_shell_spawned_turn WHERE turn_id=?1 AND CASE WHEN json_valid(shell_json) THEN json_extract(shell_json,'$.spawned')=1 ELSE 1 END LIMIT 128)",[&turn_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let tools = ThreadActivityTools {
        running_since,
        spawned: spawned as u64,
        results,
        cancelled,
        capped: groups_capped || seen == 128 || spawned < seen || cancelled == 128,
    };
    let attempt: Option<SavedAttempt> = db
        .query_row(
            "SELECT id,state,generation,CASE WHEN ?2 AND json_valid(error_json) THEN json_extract(error_json,'$.error') ELSE NULL END,CASE WHEN json_valid(error_json) THEN json_extract(error_json,'$.automatic_retry') ELSE NULL END,CASE WHEN json_valid(error_json) THEN CASE WHEN json_type(error_json,'$.automatic_retry_exhausted')='object' THEN json_extract(error_json,'$.automatic_retry_exhausted') ELSE NULL END ELSE NULL END,substr(started_at,1,128) FROM model_attempt INDEXED BY activity_attempt_turn WHERE turn_id=?1 ORDER BY rowid DESC LIMIT 1",
            rusqlite::params![turn_id, reason.is_some()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()?;
    if let Some((attempt_id, state, attempt_generation, error, retry, exhausted, started_at)) =
        attempt
    {
        let current = attempt_generation == generation;
        let mut current_attempt = ThreadActivityAttempt {
            state: decode::<AttemptState>(&state, "attempt")?,
            attempt_id,
            started_at,
            automatic_retry: None,
            retry_exhausted: None,
            error_kind: None,
            error_message: None,
            attachment_unavailable: false,
            attachment_unsupported: false,
            attachment_check_limited: false,
        };
        if current {
            // Bound the physical attempt prefix and retained error bodies before decoding.
            let (unavailable, unsupported, limited): (bool, bool, bool) = db.query_row(
                "WITH cohort AS MATERIALIZED (SELECT state,CASE WHEN octet_length(error_json)<=1024 THEN error_json END AS error FROM attachment_send WHERE attempt_id=?1 ORDER BY ordinal LIMIT 129) SELECT EXISTS(SELECT 1 FROM cohort WHERE state='unavailable'), EXISTS(SELECT 1 FROM cohort WHERE state='unavailable' AND CASE WHEN json_valid(error) THEN json_extract(error,'$.message')=?2 ELSE 0 END), (SELECT COUNT(*)=129 FROM cohort)",
                rusqlite::params![current_attempt.attempt_id, crate::attachments::UNSUPPORTED_FORMAT_ERROR],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            current_attempt.attachment_unavailable = unavailable;
            current_attempt.attachment_unsupported = unsupported;
            current_attempt.attachment_check_limited = limited;
            current_attempt.automatic_retry = retry
                .and_then(|body| serde_json::from_str::<ThreadActivityRetry>(&body).ok())
                .filter(|retry| retry.number > 0 && retry.number <= retry.limit);
            current_attempt.retry_exhausted = exhausted
                .and_then(|body| serde_json::from_str::<ThreadActivityRetryExhausted>(&body).ok())
                .filter(|counts| {
                    (1..=20).contains(&counts.limit)
                        && counts.retries >= counts.limit
                        && counts.retries <= 20
                });
            let recovery = reason
                .as_deref()
                .and_then(|reason| serde_json::from_str::<ApiError>(reason).ok());
            if current_attempt.state == AttemptState::Incomplete
                && let (Some(body), Some(recovery)) = (error, recovery)
                && let Ok(model) = serde_json::from_str::<crate::model::ModelError>(&body)
                && let Some(message) = model_failure_message(
                    &crate::config_setup::tui_error(&recovery),
                    &model,
                    current_attempt.retry_exhausted.as_ref(),
                )
            {
                current_attempt.error_kind = Some(if model.is_context_limit() {
                    ThreadActivityErrorKind::ModelContextLimit
                } else {
                    ThreadActivityErrorKind::ModelRequest
                });
                current_attempt.error_message = Some(message);
            }
        }
        activity.current_attempt = Some(current_attempt);
    }
    activity.turn = Some(ThreadActivityTurn {
        state: decode::<TurnState>(&turn_state, "Turn")?,
        turn_id,
        generation,
        recovery_reason: reason,
        tools,
        recovery_counts,
        context_reduction,
    });
    Ok(Some(activity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_counts_are_bounded_even_when_groups_or_results_are_sparse() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('u','t','r','completed','now'); INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('conversation',(SELECT thread_id FROM turn WHERE id='u'),'a','u',0,0,'p','m',0,'completed','{}','now'); INSERT INTO node(id,kind,created_at,payload_json) VALUES('n','user','now','{}');").unwrap();
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
    fn window_activity_ids_are_this_projection() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('old','t','completed','2020-01-01T00:00:00Z'),('r','t','live','2020-01-02T00:00:00Z');
            INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES('u','t','r','running',3,'2020-01-02T00:00:00Z');
            INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('conversation','t','a','u',3,0,'p','m',0,'inflight','{}','2020-01-02T00:00:00Z');").unwrap();
        let activity = read(&db, "t").unwrap().unwrap();
        assert_eq!(activity.run.run_id, "r");
        assert_eq!(activity.run.state, RunState::Live);
        let turn = activity.turn.unwrap();
        assert_eq!((turn.turn_id.as_str(), turn.generation), ("u", 3));
        assert_eq!(
            activity.current_attempt.unwrap().state,
            AttemptState::Inflight
        );
        assert!(read(&db, "missing").unwrap().is_none());
    }

    #[test]
    fn owner_matches_the_recovery_reason_to_the_attempt_model_error() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,generation,created_at)VALUES('u','t','r','recovery_wait',2,'now');").unwrap();
        let reason = |message: &str| {
            serde_json::to_string(&ApiError::new(
                crate::protocol::ErrorCode::IoFailed,
                message,
            ))
            .unwrap()
        };
        let error =
            serde_json::json!({"error":{"kind":"provider","message":"saved reason","status":500},
            "automatic_retry_exhausted":{"retries":3,"limit":3}})
            .to_string();
        db.execute("INSERT INTO model_attempt(kind,thread_id,id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES('conversation','t','a','u',2,0,'p','m',0,'incomplete','{}',?1,'now')",[error]).unwrap();
        for (saved, kind, message) in [
            (
                "provider: saved reason",
                Some(ThreadActivityErrorKind::ModelRequest),
                Some("saved reason"),
            ),
            (
                "provider: saved reason (automatic retries exhausted: 3/3)",
                Some(ThreadActivityErrorKind::ModelRequest),
                Some("saved reason (automatic retries exhausted: 3/3)"),
            ),
            ("an unrelated reason", None, None),
        ] {
            db.execute("UPDATE turn SET recovery_reason_json=?1", [reason(saved)])
                .unwrap();
            let activity = read(&db, "t").unwrap().unwrap();
            let attempt = activity.current_attempt.unwrap();
            assert_eq!(attempt.error_kind, kind, "{saved}");
            assert_eq!(attempt.error_message.as_deref(), message, "{saved}");
            assert!(activity.turn.unwrap().recovery_counts.is_some());
        }
        // A stale generation carries no generation-scoped facts.
        db.execute(
            "UPDATE turn SET generation=3, recovery_reason_json=?1",
            [reason("provider: saved reason")],
        )
        .unwrap();
        let attempt = read(&db, "t").unwrap().unwrap().current_attempt.unwrap();
        assert!(attempt.error_kind.is_none() && attempt.retry_exhausted.is_none());
    }
}
