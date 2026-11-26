//! Bounded observations of work owned by the selected saved Run.
use crate::{
    protocol::{ApiError, ErrorCode, ShellWait, WaitCondition},
    storage::StorageError,
    ui::live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget, WaitInfo, WaitTarget},
};
use rusqlite::{Connection, OptionalExtension};
use std::time::{Duration, Instant};

pub(super) const MAX_WAITS: usize = 128;
// Candidate sets are capped before sorting and counting. Counts at the cap are
// lower bounds; they cannot establish the globally oldest work. Waits are the
// owner's `shell.list` wait facts; this snapshot keeps those of the selected Run.
const LIVE_SQL: &str = r"
WITH wait_keys AS MATERIALIZED (
 SELECT DISTINCT json_extract(value,'$[0]') AS operation_id,
                 json_extract(value,'$[1]') AS execution_id,
                 json_extract(value,'$[2]') AS started_at FROM json_each(?2)
), waits AS MATERIALIZED (
 SELECT k.operation_id,k.execution_id,k.started_at
 FROM wait_keys k JOIN shell_execution s ON s.id=k.execution_id AND s.owner_run_id=?1
), shell_candidates AS MATERIALIZED (
 SELECT id,started_at FROM shell_execution INDEXED BY shell_active_owner
 WHERE owner_run_id=?1 AND state IN ('starting','running','stopping') LIMIT 128
), shell_members AS MATERIALIZED (
 SELECT id,started_at FROM shell_candidates
 WHERE NOT EXISTS (SELECT 1 FROM waits WHERE waits.execution_id=shell_candidates.id)
 ORDER BY julianday(started_at) IS NULL,julianday(started_at),id LIMIT ?3
), shells AS MATERIALIZED (
 SELECT 'shell' AS kind,s.id AS id,s.turn_id AS related,s.id AS execution,
        CASE WHEN json_valid(s.shell_json) THEN CASE WHEN json_type(s.shell_json,'$.command')='text' THEN substr(json_extract(s.shell_json,'$.command'),1,512) END END AS label,
        s.state,s.started_at,
        (SELECT received_at FROM output_chunk o WHERE o.execution_id=s.id ORDER BY o.sequence DESC LIMIT 1) AS output_at,
        CASE WHEN json_valid(s.shell_json) THEN CASE WHEN json_type(s.shell_json,'$.description')='text' THEN substr(json_extract(s.shell_json,'$.description'),1,80) END END AS description
 FROM shell_members member CROSS JOIN shell_execution s ON s.id=member.id
), wait_rows AS MATERIALIZED (
 SELECT 'wait' AS kind,operation_id AS id,NULL AS related,execution_id AS execution,
        NULL AS label,'waiting' AS state,started_at,NULL AS output_at,NULL AS description FROM waits
 ORDER BY julianday(started_at) IS NULL,julianday(started_at),operation_id LIMIT ?3
), picked AS MATERIALIZED (
 SELECT * FROM (SELECT * FROM shells UNION ALL SELECT * FROM wait_rows)
 ORDER BY julianday(started_at) IS NULL,julianday(started_at),id,kind LIMIT ?3
), counts AS MATERIALIZED (
 SELECT (SELECT count(*) FROM shell_candidates) AS shells,
        (SELECT count(*) FROM waits) AS waits,
        (SELECT count(DISTINCT s.id) FROM waits w JOIN shell_execution s ON s.id=w.execution_id
         WHERE s.owner_run_id=?1 AND s.state IN ('starting','running','stopping')) AS duplicate_shells
)
SELECT picked.kind,picked.id,picked.related,picked.execution,picked.label,picked.state,picked.started_at,picked.output_at,
       counts.shells,counts.waits,picked.description,counts.duplicate_shells
FROM counts LEFT JOIN picked ON 1
ORDER BY julianday(picked.started_at) IS NULL,julianday(picked.started_at),picked.id,picked.kind";

pub(super) struct RawLiveWork {
    run_id: String,
    rows: Vec<RawRow>,
    shells: u64,

    waits: Option<u64>,
    known_waits: u64,
    duplicate_shells: u64,
    observed_at: Instant,
    wall_at: chrono::DateTime<chrono::Utc>,
}
struct RawRow {
    kind: String,
    id: String,
    related: Option<String>,
    execution: Option<String>,
    label: Option<String>,
    description: Option<String>,
    state: String,
    started_at: Option<String>,
    output_at: Option<String>,
    title: Option<super::header::TitleWitness>,
    wait: Option<ShellWait>,
    wait_target: Option<WaitTarget>,
}

/// Owner-proved waits of the selected Thread's running shells, by execution;
/// `None` when the owner's listing did not cover every running shell.
pub(super) type ShellWaits = Option<Vec<(String, ShellWait)>>;

#[cfg(test)]
pub(super) fn read_tx(
    db: &Connection,
    run_id: &str,
    waits: Option<&[(String, ShellWait)]>,
) -> Result<RawLiveWork, StorageError> {
    read_tx_limited(db, run_id, waits, 3)
}

pub(super) fn read_tx_limited(
    db: &Connection,
    run_id: &str,
    waits: Option<&[(String, ShellWait)]>,
    limit: usize,
) -> Result<RawLiveWork, StorageError> {
    if !(1..=10).contains(&limit) {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Live work row limit must be in 1..=10",
        )
        .into());
    }
    if waits.is_some_and(|scope| scope.len() > MAX_WAITS) {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Live wait source scope exceeds128 entries",
        )
        .into());
    }
    let known = waits.is_some();
    let scope = waits.unwrap_or_default();
    let encoded = serde_json::to_string(
        &scope
            .iter()
            .map(|(execution, wait)| [&wait.operation_id, execution, &wait.accepted_at])
            .collect::<Vec<_>>(),
    )
    .expect("wait identities serialize");
    let mut result = RawLiveWork {
        run_id: run_id.into(),
        rows: Vec::new(),
        shells: 0,

        waits: known.then_some(0),
        known_waits: 0,
        duplicate_shells: 0,
        observed_at: Instant::now(),
        wall_at: chrono::Utc::now(),
    };
    let mut statement = db.prepare(LIVE_SQL)?;
    let mut rows = statement.query(rusqlite::params![run_id, encoded, limit as i64])?;
    while let Some(row) = rows.next()? {
        result.shells = row.get::<_, i64>(8)? as u64;
        result.known_waits = row.get::<_, i64>(9)? as u64;
        result.duplicate_shells = row.get::<_, i64>(11)? as u64;
        result.waits = known.then_some(result.known_waits);
        if let Some(kind) = row.get::<_, Option<String>>(0)? {
            let id: String = row.get(1)?;
            let wait = (kind == "wait")
                .then(|| {
                    scope
                        .iter()
                        .find(|(_, wait)| wait.operation_id == id)
                        .map(|(_, wait)| wait.clone())
                })
                .flatten();
            result.rows.push(RawRow {
                kind,
                id,
                related: row.get(2)?,
                execution: row.get(3)?,
                label: row.get(4)?,
                description: row.get(10)?,
                state: row.get(5)?,
                started_at: row.get(6)?,
                output_at: row.get(7)?,
                title: None,
                wait,
                wait_target: None,
            });
        }
    }
    drop(rows);
    drop(statement);
    // Resolve only the selected wait targets by primary key in this snapshot.
    for row in &mut result.rows {
        let Some(condition) = row.wait.as_ref().map(|wait| &wait.first_condition) else {
            continue;
        };
        match condition {
            WaitCondition::ShellEnded { execution_id }
            | WaitCondition::ShellOutput { execution_id, .. } => {
                row.wait_target = Some(WaitTarget::Shell(execution_id.clone()));
                row.label = db.query_row(
                    "SELECT CASE WHEN json_type(shell_json,'$.command')='text' THEN substr(json_extract(shell_json,'$.command'),1,512) END FROM shell_execution WHERE id=?1",
                    [execution_id], |r| r.get(0),
                ).optional()?.flatten();
            }
            WaitCondition::RunEnded { run_id: id }
            | WaitCondition::TurnCompleted { turn_id: id } => {
                let sql = if matches!(condition, WaitCondition::RunEnded { .. }) {
                    "SELECT t.id,substr(t.title,1,512) FROM run r JOIN thread t ON t.id=r.thread_id WHERE r.id=?1"
                } else {
                    "SELECT t.id,substr(t.title,1,512) FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=?1"
                };
                let found: Option<(String, String)> = db
                    .query_row(sql, [id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                if let Some((thread, title)) = found {
                    row.wait_target =
                        Some(if matches!(condition, WaitCondition::RunEnded { .. }) {
                            WaitTarget::Run(id.clone())
                        } else {
                            WaitTarget::Thread(thread.clone())
                        });
                    row.title = super::header::read_title_tx(db, &thread, &title)?;
                    row.label = Some(title);
                }
            }
            WaitCondition::ThreadSettled { thread_ids, .. } => {
                let Some(id) = thread_ids.first() else {
                    continue;
                };
                let found: Option<(String, String)> = db
                    .query_row(
                        "SELECT id,substr(title,1,512) FROM thread WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((thread, title)) = found {
                    row.wait_target = Some(WaitTarget::Thread(thread.clone()));
                    row.title = super::header::read_title_tx(db, &thread, &title)?;
                    row.label = Some(title);
                }
            }
            WaitCondition::MessageDelivered { .. } | WaitCondition::ReplyReceived { .. } => {}
        }
    }
    // Capture after the last same-snapshot child/title observation and before
    // yielding the DB worker: later output commits must remain newer markers.
    result.observed_at = Instant::now();
    result.wall_at = chrono::Utc::now();
    Ok(result)
}

pub(super) fn age(saved: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let saved = chrono::DateTime::parse_from_rfc3339(saved?).ok()?;
    now.signed_duration_since(saved).to_std().ok()
}
pub(super) fn label(value: Option<&str>, fallback: &str) -> String {
    let (safe, _) = crate::presentation::sanitize_plain(value.unwrap_or_default(), 0);
    let safe: String = safe.chars().take(512).collect();
    if safe.trim().is_empty() {
        fallback.into()
    } else {
        safe
    }
}

#[cfg(test)]
pub(super) fn prepare(raw: RawLiveWork) -> LiveWorkSnapshot {
    prepare_in(raw, crate::config::UiLanguage::Ja)
}

pub(super) fn prepare_in(
    raw: RawLiveWork,
    language: crate::config::UiLanguage,
) -> LiveWorkSnapshot {
    let observed_at = raw.observed_at;
    let now = raw.wall_at;
    let overflow = raw
        .shells
        .saturating_add(raw.known_waits)
        .saturating_sub(raw.duplicate_shells)
        .saturating_sub(raw.rows.len() as u64);
    let rows = raw
        .rows
        .into_iter()
        .map(|mut row| {
            if let Some(title) = row.title.take() {
                row.label = Some(title.prepare());
            }
            let wait_info = (row.kind == "wait").then(|| WaitInfo {
                target: row.wait_target.take(),
                timeout: row
                    .wait
                    .as_ref()
                    .map(|wait| Duration::from_millis(wait.timeout_ms)),
            });
            let (target, display) = match row.kind.as_str() {
                "shell" => (
                    LiveWorkTarget::Shell {
                        execution_id: row.id,
                        turn_id: row.related,
                    },
                    label(
                        row.label.as_deref(),
                        crate::ui::i18n::local!(
                            language,
                            "コマンド（内容未記録）",
                            "Command (not recorded)"
                        ),
                    ),
                ),
                _ => (
                    LiveWorkTarget::Wait {
                        operation_id: row.id,
                        execution_id: row.execution.expect("validated wait source"),
                    },
                    {
                        let condition = row.wait.as_ref().map(|wait| &wait.first_condition);
                        let kind = match condition {
                            Some(WaitCondition::ShellEnded { .. }) => {
                                crate::ui::i18n::local!(language, "shell の終了", "Shell ended")
                            }
                            Some(WaitCondition::ShellOutput { .. }) => {
                                crate::ui::i18n::local!(language, "shell の出力", "Shell output")
                            }
                            Some(WaitCondition::TurnCompleted { .. }) => {
                                crate::ui::i18n::local!(language, "子の回答", "Child response")
                            }
                            Some(WaitCondition::RunEnded { .. }) => {
                                crate::ui::i18n::local!(language, "Run の終了", "Run ended")
                            }
                            Some(WaitCondition::ThreadSettled { .. }) => crate::ui::i18n::local!(
                                language,
                                "会話の状態",
                                "Conversation state"
                            ),
                            _ => crate::ui::i18n::local!(
                                language,
                                "待機対象不明",
                                "Unknown wait target"
                            ),
                        };
                        let name = label(
                            row.label.as_deref(),
                            crate::ui::i18n::local!(language, "対象不明", "Unknown target"),
                        );
                        let others = row
                            .wait
                            .as_ref()
                            .map_or(0, |wait| wait.condition_count)
                            .saturating_sub(1);
                        let other_threads = match condition {
                            Some(WaitCondition::ThreadSettled { thread_ids, .. }) => {
                                thread_ids.len().saturating_sub(1)
                            }
                            _ => 0,
                        };
                        let thread_summary = if other_threads > 0 {
                            match language {
                                crate::config::UiLanguage::Ja => {
                                    crate::ui::text_width::glyphs::formatted!(
                                        " · 他{other_threads}会話",
                                        " | 他{other_threads}会話"
                                    )
                                }
                                crate::config::UiLanguage::En => {
                                    crate::ui::text_width::glyphs::formatted!(
                                        " · {other_threads} other threads",
                                        " | {other_threads} other threads"
                                    )
                                }
                            }
                        } else {
                            String::new()
                        };
                        crate::ui::text_width::glyphs::formatted!(
                            "{kind} · {name}{thread_summary}{}",
                            "{kind} | {name}{thread_summary}{}",
                            if others > 0 {
                                match language {
                                    crate::config::UiLanguage::Ja => {
                                        crate::ui::text_width::glyphs::formatted!(
                                            " · 他{others}件",
                                            " | 他{others}件"
                                        )
                                    }
                                    crate::config::UiLanguage::En => {
                                        crate::ui::text_width::glyphs::formatted!(
                                            " · {others} more",
                                            " | {others} more"
                                        )
                                    }
                                }
                            } else {
                                String::new()
                            }
                        )
                    },
                ),
            };
            LiveWorkRow {
                target,
                label: display,
                description: crate::tool_description::label(row.description.as_deref()),
                state: row.state,
                elapsed_base: age(row.started_at.as_deref(), now),
                output_age_base: age(row.output_at.as_deref(), now),
                started_at: row.started_at,
                last_output_at: row.output_at,
                wait_info,
            }
        })
        .collect();
    LiveWorkSnapshot {
        run_id: raw.run_id,
        rows,
        shells: raw.shells,

        waits: raw.waits,
        overflow,
        counts_capped: raw.shells == 128,
        observed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'),('child','/','','p','m','now'),('lineage','/','','p','m','now');
            UPDATE thread SET parent_thread_id='t' WHERE id='lineage';
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2020-01-01T00:00:00Z'),('lineage-run','lineage','live','2010-01-01T00:00:00Z');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('child-run','child','live','2020-01-01T00:00:01Z');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2020-01-01T00:00:00Z');").unwrap();
        for (id, owner, start) in [
            ("s-old", "r", "2020-01-01T00:00:00Z"),
            ("s-new", "r", "2020-01-01T00:00:03Z"),
            ("s-third", "r", "2020-01-01T00:00:04Z"),
            ("s-fourth", "r", "2020-01-01T00:00:05Z"),
            ("foreign", "lineage-run", "2010-01-01T00:00:00Z"),
        ] {
            db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES(?1,?2,'u',?1,'running','/',?3,'UTF-8','UTF-8','open',?4)",rusqlite::params![id,owner,json!({"command":"printf saved\u{1b}[31m command"}).to_string(),start]).unwrap();
        }
        db.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('s-old',0,'stdout',0,x'61','2020-01-01T00:00:02Z'),('s-old',1,'stderr',0,x'62','2020-01-01T00:00:09Z')",[]).unwrap();
        db
    }
    /// One owner `shell.list` wait fact for `execution`.
    fn wait(
        operation: &str,
        execution: &str,
        conditions: &[serde_json::Value],
        timeout_ms: u64,
    ) -> (String, ShellWait) {
        (
            execution.into(),
            ShellWait {
                operation_id: operation.into(),
                accepted_at: "2020-01-01T00:00:02Z".into(),
                timeout_ms,
                condition_count: conditions.len() as u32,
                first_condition: serde_json::from_value(conditions[0].clone()).unwrap(),
            },
        )
    }
    fn message_wait(operation: &str, execution: &str) -> (String, ShellWait) {
        wait(
            operation,
            execution,
            &[json!({"type":"message_delivered","message_id":"m"})],
            30000,
        )
    }

    #[test]
    fn row_limit_bounds_the_candidate_merge_without_changing_counts() {
        let db = fixture();
        for (limit, rows, overflow) in [(1, 1, 3), (3, 3, 1), (10, 4, 0)] {
            let snapshot = prepare(read_tx_limited(&db, "r", Some(&[]), limit).unwrap());
            assert_eq!(snapshot.rows.len(), rows);
            assert_eq!(snapshot.overflow, overflow);
            assert_eq!(snapshot.shells, 4);
        }
        for invalid in [0, 11] {
            assert!(read_tx_limited(&db, "r", Some(&[]), invalid).is_err());
        }
    }
    #[test]
    fn live_work_plan_seeks_owned_candidates_and_exact_child_state() {
        let db = fixture();
        let mut statement = db
            .prepare(&format!("EXPLAIN QUERY PLAN {LIVE_SQL}"))
            .unwrap();
        let details: Vec<String> = statement
            .query_map(rusqlite::params!["r", "[]", 3], |row| row.get(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        {
            let index = "shell_active_owner";
            assert!(
                details.iter().any(|detail| detail.contains(index)),
                "missing {index}: {details:?}"
            );
        }
        assert!(
            !details
                .iter()
                .any(|detail| detail == "SCAN shell_execution" || detail == "SCAN run"),
            "{details:?}"
        );
    }

    #[test]
    fn large_owned_scope_reports_a_lower_bound_without_claiming_global_order() {
        let db = fixture();
        for i in 0..130 {
            db.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES(?1,'r','u',?1,'running','/','{}','UTF-8','UTF-8','open','2010-01-01T00:00:00Z')", [format!("many-{i:03}")]).unwrap();
        }
        let snapshot = prepare(read_tx_limited(&db, "r", Some(&[]), 3).unwrap());
        assert_eq!(snapshot.shells, 128);
        assert!(snapshot.counts_capped);
        assert_eq!(snapshot.rows.len(), 3);
        assert!(snapshot.overflow <= 131);
    }

    #[test]
    fn selected_wait_describes_first_saved_target_and_avoids_duplicate_caller() {
        let db = fixture();
        let scope = [wait(
            "w",
            "s-old",
            &[
                json!({"type":"shell_ended","execution_id":"s-new"}),
                json!({"type":"run_ended","run_id":"child-run"}),
            ],
            300000,
        )];
        let snapshot = prepare(read_tx_limited(&db, "r", Some(&scope), 10).unwrap());
        assert_eq!(
            (snapshot.shells, snapshot.waits, snapshot.overflow),
            (4, Some(1), 0)
        );
        assert!(!snapshot.rows.iter().any(|r| matches!(&r.target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == "s-old")));
        let wait = snapshot
            .rows
            .iter()
            .find(|r| matches!(r.target, LiveWorkTarget::Wait { .. }))
            .unwrap();
        assert!(
            wait.label.contains("shell の終了")
                && wait.label.contains("printf saved")
                && wait.label.contains("他1件"),
            "{}",
            wait.label
        );
        assert_eq!(
            wait.wait_info,
            Some(WaitInfo {
                target: Some(WaitTarget::Shell("s-new".into())),
                timeout: Some(Duration::from_secs(300))
            })
        );
        let english = prepare_in(
            read_tx_limited(&db, "r", Some(&scope), 10).unwrap(),
            crate::config::UiLanguage::En,
        );
        let english_wait = english
            .rows
            .iter()
            .find(|row| matches!(row.target, LiveWorkTarget::Wait { .. }))
            .unwrap();
        assert!(
            english_wait.label.contains("Shell ended"),
            "{}",
            english_wait.label
        );
        assert!(
            english_wait.label.contains("1 more"),
            "{}",
            english_wait.label
        );
        assert!(
            english_wait.label.contains("printf saved"),
            "{}",
            english_wait.label
        );
        assert!(
            !english_wait.label.contains("shell の終了"),
            "{}",
            english_wait.label
        );
    }
    #[test]
    fn waited_child_uses_saved_title_and_the_correct_condition_field() {
        let db = fixture();
        db.execute(
            "UPDATE thread SET title='Child investigation' WHERE id='child'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('child-turn','child','child-run','running','2020-01-01T00:00:01Z')", []).unwrap();
        let scope = [wait(
            "w",
            "s-old",
            &[json!({"type":"turn_completed","turn_id":"child-turn"})],
            30000,
        )];
        let snapshot = prepare(read_tx_limited(&db, "r", Some(&scope), 10).unwrap());
        let wait = snapshot
            .rows
            .iter()
            .find(|r| matches!(r.target, LiveWorkTarget::Wait { .. }))
            .unwrap();
        assert!(
            wait.label.contains("子の回答") && wait.label.contains("Child investigation"),
            "{}",
            wait.label
        );
        assert_eq!(
            wait.wait_info.as_ref().unwrap().target,
            Some(WaitTarget::Thread("child".into()))
        );
    }
    #[test]
    fn multiple_thread_targets_are_summarized_and_missing_target_stays_unknown() {
        let db = fixture();
        db.execute("UPDATE thread SET title='First child' WHERE id='child'", [])
            .unwrap();
        for (condition, expected) in [
            (
                json!({"type":"thread_settled", "thread_ids":["child","lineage"], "states":["completed"]}),
                "First child | 他1会話",
            ),
            (
                json!({"type":"run_ended", "run_id":"missing-run"}),
                "対象不明",
            ),
        ] {
            let scope = [wait("w", "s-old", &[condition], 30000)];
            let snapshot = prepare(read_tx_limited(&db, "r", Some(&scope), 10).unwrap());
            let wait = snapshot
                .rows
                .iter()
                .find(|r| matches!(r.target, LiveWorkTarget::Wait { .. }))
                .unwrap();
            assert!(wait.label.contains(expected), "{}", wait.label);
        }
    }
    #[test]
    fn common_description_metadata_preserves_real_command_label_for_details_and_optout() {
        let db = fixture();
        db.execute("UPDATE shell_execution SET shell_json=json_set(shell_json,'$.description',?1) WHERE id='s-old'", ["  Inspect\nfiles\u{7}  "]).unwrap();
        let snapshot = prepare(read_tx(&db, "r", Some(&[])).unwrap());
        let first = &snapshot.rows[0];
        assert_eq!(first.description.as_deref(), Some("Inspect files"));
        assert!(first.label.contains("printf saved"));
        assert!(
            matches!(&first.target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == "s-old")
        );
    }
    #[test]
    fn saved_ownership_oldest_rows_counts_and_timing_survive_later_changes() {
        let db = fixture();
        let scope = [message_wait("wait-old", "s-old")];
        let raw = read_tx(&db, "r", Some(&scope)).unwrap();
        db.execute("UPDATE shell_execution SET state='exited'", [])
            .unwrap();
        drop(db);
        let snapshot = prepare(raw);
        assert_eq!(
            (snapshot.shells, snapshot.waits, snapshot.overflow),
            (4, Some(1), 1)
        );
        assert_eq!(snapshot.rows.len(), 3);
        assert!(snapshot.rows.iter().all(|row| !row.label.contains("child")));
        assert!(
            matches!(&snapshot.rows[0].target, LiveWorkTarget::Wait { operation_id, .. } if operation_id == "wait-old")
        );
        assert!(
            matches!(&snapshot.rows[1].target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == "s-new")
        );
    }

    #[test]
    fn owner_waits_count_only_for_the_selected_run() {
        let db = fixture();
        let scope = vec![
            message_wait("valid", "s-old"),
            message_wait("foreign", "foreign"),
            message_wait("unknown-shell", "missing"),
        ];
        let observed = prepare(read_tx(&db, "r", Some(&scope)).unwrap());
        assert_eq!(observed.waits, Some(1));
        assert_eq!(prepare(read_tx(&db, "r", None).unwrap()).waits, None);
        assert_eq!(
            prepare(read_tx(&db, "r", Some(&[])).unwrap()).waits,
            Some(0)
        );
        // The owner's wait fact, not this snapshot's shell state, decides it.
        db.execute(
            "UPDATE shell_execution SET state='exited' WHERE id='s-old'",
            [],
        )
        .unwrap();
        assert_eq!(
            prepare(read_tx(&db, "r", Some(&scope)).unwrap()).waits,
            Some(1)
        );
        assert!(read_tx(&db, "r", Some(&vec![message_wait("a", "b"); 129])).is_err());
    }

    #[test]
    fn invalid_or_future_saved_times_stay_unknown_and_labels_are_bounded() {
        assert!(age(Some("invalid"), chrono::Utc::now()).is_none());
        assert!(age(Some("2999-01-01T00:00:00Z"), chrono::Utc::now()).is_none());
        assert!(age(None, chrono::Utc::now()).is_none());
        assert_eq!(
            label(Some(&"x".repeat(1000)), "fallback").chars().count(),
            512
        );
    }

    #[test]
    fn child_title_and_recovery_reuse_saved_bounded_header_and_activity_facts() {
        let db = fixture();
        db.execute_batch("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('child-turn','child','child-run','recovery_wait','2020-01-01T00:00:01Z');
            INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('child-question','child-turn','user','now','{\"role\":\"user\",\"content\":\"Known child question\"}');
            INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES('child',0,'child-question');").unwrap();
        let raw = read_tx(&db, "r", Some(&[])).unwrap();
        db.execute(
            "UPDATE node SET payload_json=NULL WHERE id='child-question'",
            [],
        )
        .unwrap();
        assert!(prepare(raw).rows.iter().all(|row| matches!(
            row.target,
            LiveWorkTarget::Shell { .. } | LiveWorkTarget::Wait { .. }
        )));
    }

    #[test]
    fn chronological_display_handles_fractional_and_offset_times_with_unknown_last() {
        let db = fixture();
        db.execute_batch(
            "UPDATE shell_execution SET started_at='invalid' WHERE owner_run_id='r';
            UPDATE shell_execution SET started_at='2020-01-01T00:00:00Z' WHERE id='s-old';
            UPDATE shell_execution SET started_at='2020-01-01T00:00:00.1Z' WHERE id='s-new';
            UPDATE run SET created_at='2020-01-01T01:00:00.2+01:00' WHERE id='child-run';",
        )
        .unwrap();
        let snapshot = prepare(read_tx(&db, "r", Some(&[])).unwrap());
        assert!(
            matches!(&snapshot.rows[0].target,LiveWorkTarget::Shell{execution_id,..}if execution_id=="s-old")
        );
        assert!(
            matches!(&snapshot.rows[1].target,LiveWorkTarget::Shell{execution_id,..}if execution_id=="s-new")
        );
        assert!(matches!(
            &snapshot.rows[2].target,
            LiveWorkTarget::Shell { .. }
        ));
    }

    #[test]
    fn delayed_preparation_keeps_snapshot_clock_before_new_output_markers() {
        let db = fixture();
        let raw = read_tx(&db, "r", Some(&[])).unwrap();
        let observed_at = raw.observed_at;
        let expected_elapsed = age(Some("2020-01-01T00:00:00Z"), raw.wall_at);
        // Preparation may run later on the presentation worker; it keeps the
        // DB snapshot's clock, so a newer commit marker still supersedes it.
        let prepared = prepare(raw);
        assert_eq!(prepared.observed_at, observed_at);
        assert_eq!(prepared.rows[0].elapsed_base, expected_elapsed);
    }
}
