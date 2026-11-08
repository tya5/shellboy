//! Bounded observations of work owned by the selected saved Run.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget, WaitInfo, WaitTarget},
};
use rusqlite::{Connection, OptionalExtension};
use std::time::{Duration, Instant};

const MAX_WAITS: usize = 128;
// Candidate sets are capped before sorting and counting. Counts at the cap are
// lower bounds; they cannot establish the globally oldest work.
const LIVE_SQL: &str = r"
WITH wait_keys AS MATERIALIZED (
 SELECT DISTINCT json_extract(value,'$[0]') AS operation_id,
                 json_extract(value,'$[1]') AS execution_id FROM json_each(?2)
), wait_observations AS MATERIALIZED (
 SELECT k.operation_id,k.execution_id,m.value,
        CASE WHEN json_valid(m.value) THEN
            json_type(m.value,'$.operation_id')='text'
            AND json_type(m.value,'$.caller.run_id')='text'
            AND json_type(m.value,'$.caller.execution_id')='text' ELSE 0 END AS proof_valid
 FROM wait_keys k JOIN shell_execution s ON s.id=k.execution_id AND s.owner_run_id=?1
 JOIN app_meta m ON m.key='wait.pending.'||k.operation_id
), waits AS MATERIALIZED (
 SELECT operation_id,execution_id,value,
        CASE WHEN json_valid(value) THEN CASE WHEN json_type(value,'$.accepted_at')='text' THEN json_extract(value,'$.accepted_at') END END AS started_at
 FROM wait_observations
 WHERE CASE WHEN proof_valid THEN json_extract(value,'$.operation_id')=operation_id
     AND json_extract(value,'$.caller.run_id')=?1
     AND json_extract(value,'$.caller.execution_id')=execution_id ELSE 0 END
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
        0 AS children,
        (SELECT count(*) FROM waits) AS waits,
        (SELECT count(*) FROM wait_observations WHERE COALESCE(proof_valid,0)=0) AS invalid_waits,
        (SELECT count(DISTINCT s.id) FROM waits w JOIN shell_execution s ON s.id=w.execution_id
         WHERE s.owner_run_id=?1 AND s.state IN ('starting','running','stopping')) AS duplicate_shells
)
SELECT picked.kind,picked.id,picked.related,picked.execution,picked.label,picked.state,picked.started_at,picked.output_at,
       counts.shells,counts.children,counts.waits,counts.invalid_waits,
       picked.description,counts.duplicate_shells,
       CASE WHEN json_type(w.value,'$.conditions')='array' THEN json_array_length(w.value,'$.conditions') END,
       CASE WHEN json_type(w.value,'$.conditions[0].type')='text' THEN substr(json_extract(w.value,'$.conditions[0].type'),1,64) END,
       CASE WHEN json_type(w.value,'$.timeout_ms')='integer' AND typeof(json_extract(w.value,'$.timeout_ms'))='integer' AND json_extract(w.value,'$.timeout_ms')>=0 THEN json_extract(w.value,'$.timeout_ms') END,
       CASE WHEN json_extract(w.value,'$.conditions[0].type') IN ('shell_ended','shell_output') AND json_type(w.value,'$.conditions[0].execution_id')='text' AND octet_length(json_extract(w.value,'$.conditions[0].execution_id'))<=512 THEN json_extract(w.value,'$.conditions[0].execution_id')
            WHEN json_extract(w.value,'$.conditions[0].type')='run_ended' AND json_type(w.value,'$.conditions[0].run_id')='text' AND octet_length(json_extract(w.value,'$.conditions[0].run_id'))<=512 THEN json_extract(w.value,'$.conditions[0].run_id')
            WHEN json_extract(w.value,'$.conditions[0].type')='turn_completed' AND json_type(w.value,'$.conditions[0].turn_id')='text' AND octet_length(json_extract(w.value,'$.conditions[0].turn_id'))<=512 THEN json_extract(w.value,'$.conditions[0].turn_id')
            WHEN json_extract(w.value,'$.conditions[0].type')='thread_settled' AND json_type(w.value,'$.conditions[0].thread_ids[0]')='text' AND octet_length(json_extract(w.value,'$.conditions[0].thread_ids[0]'))<=512 THEN json_extract(w.value,'$.conditions[0].thread_ids[0]') END,
       CASE WHEN json_extract(w.value,'$.conditions[0].type')='thread_settled' AND json_type(w.value,'$.conditions[0].thread_ids')='array' THEN json_array_length(w.value,'$.conditions[0].thread_ids') END
FROM counts LEFT JOIN picked ON 1
LEFT JOIN waits w ON picked.kind='wait' AND w.operation_id=picked.id
ORDER BY julianday(picked.started_at) IS NULL,julianday(picked.started_at),picked.id,picked.kind";

pub(super) struct RawLiveWork {
    run_id: String,
    rows: Vec<RawRow>,
    shells: u64,
    children: u64,
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
    wait_condition_count: Option<i64>,
    wait_kind: Option<String>,
    wait_timeout_ms: Option<i64>,
    wait_target_id: Option<String>,
    wait_thread_count: Option<i64>,
    wait_target: Option<WaitTarget>,
}

#[cfg(test)]
pub(super) fn read_tx(
    db: &Connection,
    run_id: &str,
    waits: Option<&[(String, String)]>,
) -> Result<RawLiveWork, StorageError> {
    read_tx_limited(db, run_id, waits, 3)
}

pub(super) fn read_tx_limited(
    db: &Connection,
    run_id: &str,
    waits: Option<&[(String, String)]>,
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
    let encoded =
        serde_json::to_string(waits.unwrap_or_default()).expect("wait source identities serialize");
    let mut result = RawLiveWork {
        run_id: run_id.into(),
        rows: Vec::new(),
        shells: 0,
        children: 0,
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
        result.children = row.get::<_, i64>(9)? as u64;
        result.known_waits = row.get::<_, i64>(10)? as u64;
        result.duplicate_shells = row.get::<_, i64>(18)? as u64;
        result.waits = (known && row.get::<_, i64>(11)? == 0).then_some(result.known_waits);
        if let Some(kind) = row.get::<_, Option<String>>(0)? {
            result.rows.push(RawRow {
                kind,
                id: row.get(1)?,
                related: row.get(2)?,
                execution: row.get(3)?,
                label: row.get(4)?,
                description: row.get(12)?,
                state: row.get(5)?,
                started_at: row.get(6)?,
                output_at: row.get(7)?,
                title: None,
                wait_condition_count: row.get(14)?,
                wait_kind: row.get(15)?,
                wait_timeout_ms: row.get(16)?,
                wait_target_id: row.get(17)?,
                wait_thread_count: row.get(18)?,
                wait_target: None,
            });
        }
    }
    drop(rows);
    drop(statement);
    // Resolve only the selected wait targets by primary key in this snapshot.
    for row in &mut result.rows {
        let (Some(kind), Some(id)) = (row.wait_kind.as_deref(), row.wait_target_id.as_deref())
        else {
            continue;
        };
        match kind {
            "shell_ended" | "shell_output" => {
                row.wait_target = Some(WaitTarget::Shell(id.to_owned()));
                row.label = db.query_row(
                    "SELECT CASE WHEN json_type(shell_json,'$.command')='text' THEN substr(json_extract(shell_json,'$.command'),1,512) END FROM shell_execution WHERE id=?1",
                    [id], |r| r.get(0),
                ).optional()?.flatten();
            }
            "run_ended" | "turn_completed" | "thread_settled" => {
                let sql = match kind {
                    "run_ended" => {
                        "SELECT t.id,substr(t.title,1,512) FROM run r JOIN thread t ON t.id=r.thread_id WHERE r.id=?1"
                    }
                    "turn_completed" => {
                        "SELECT t.id,substr(t.title,1,512) FROM turn u JOIN thread t ON t.id=u.thread_id WHERE u.id=?1"
                    }
                    _ => "SELECT id,substr(title,1,512) FROM thread WHERE id=?1",
                };
                let found: Option<(String, String)> = db
                    .query_row(sql, [id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                if let Some((thread, title)) = found {
                    row.wait_target = Some(if kind == "run_ended" {
                        WaitTarget::Run(id.to_owned())
                    } else {
                        WaitTarget::Thread(thread.clone())
                    });
                    row.title = super::header::read_title_tx(db, &thread, &title)?;
                    row.label = Some(title);
                }
            }
            _ => {}
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
        .saturating_add(raw.children)
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
                    .wait_timeout_ms
                    .and_then(|ms| u64::try_from(ms).ok())
                    .map(Duration::from_millis),
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
                "child" => (
                    LiveWorkTarget::ChildRun {
                        run_id: row.id,
                        thread_id: row.related.expect("saved child Thread"),
                    },
                    label(
                        row.label.as_deref(),
                        crate::ui::i18n::local!(language, "名称未設定", "Untitled"),
                    ),
                ),
                _ => (
                    LiveWorkTarget::Wait {
                        operation_id: row.id,
                        execution_id: row.execution.expect("validated wait source"),
                    },
                    {
                        let kind = match row.wait_kind.as_deref() {
                            Some("shell_ended") => {
                                crate::ui::i18n::local!(language, "shell の終了", "Shell ended")
                            }
                            Some("shell_output") => {
                                crate::ui::i18n::local!(language, "shell の出力", "Shell output")
                            }
                            Some("turn_completed") => {
                                crate::ui::i18n::local!(language, "子の回答", "Child response")
                            }
                            Some("run_ended") => {
                                crate::ui::i18n::local!(language, "Run の終了", "Run ended")
                            }
                            Some("thread_settled") => crate::ui::i18n::local!(
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
                        let others = row.wait_condition_count.unwrap_or(0).saturating_sub(1);
                        let other_threads = row.wait_thread_count.unwrap_or(0).saturating_sub(1);
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
        children: raw.children,
        waits: raw.waits,
        overflow,
        counts_capped: raw.shells == 128 || raw.children == 128,
        observed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use serde_json::json;

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now'),('child','/','','p','m','now'),('lineage','/','','p','m','now');
            UPDATE thread SET parent_thread_id='t' WHERE id='lineage';
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','2020-01-01T00:00:00Z'),('lineage-run','lineage','live','2010-01-01T00:00:00Z');
            INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES('child-run','child','r','live','2020-01-01T00:00:01Z');
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
    fn pending(db: &Connection, operation: &str, execution: &str, run: &str) {
        db.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2)",rusqlite::params![format!("wait.pending.{operation}"),json!({"operation_id":operation,"caller":{"run_id":run,"execution_id":execution},"accepted_at":"2020-01-01T00:00:02Z"}).to_string()]).unwrap();
    }

    #[test]
    fn row_limit_bounds_the_candidate_merge_without_changing_counts() {
        let db = fixture();
        for (limit, rows, overflow) in [(1, 1, 4), (3, 3, 2), (10, 5, 0)] {
            let snapshot = prepare(read_tx_limited(&db, "r", Some(&[]), limit).unwrap());
            assert_eq!(snapshot.rows.len(), rows);
            assert_eq!(snapshot.overflow, overflow);
            assert_eq!((snapshot.shells, snapshot.children), (4, 1));
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
        for index in [
            "shell_active_owner",
            "runs_by_owner",
            "activity_turn_run",
            "activity_attempt_turn",
            "activity_group_turn_state",
        ] {
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
        pending(&db, "w", "s-old", "r");
        let value = json!({"operation_id":"w", "caller":{"run_id":"r","execution_id":"s-old"},
            "accepted_at":"2020-01-01T00:00:02Z", "timeout_ms":300000,
            "conditions":[{"type":"shell_ended","execution_id":"s-new"}, {"type":"run_ended","run_id":"child-run"}]}).to_string();
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='wait.pending.w'",
            [&value],
        )
        .unwrap();
        let snapshot =
            prepare(read_tx_limited(&db, "r", Some(&[("w".into(), "s-old".into())]), 10).unwrap());
        assert_eq!(
            (
                snapshot.shells,
                snapshot.children,
                snapshot.waits,
                snapshot.overflow
            ),
            (4, 1, Some(1), 0)
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
            read_tx_limited(&db, "r", Some(&[("w".into(), "s-old".into())]), 10).unwrap(),
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
        pending(&db, "w", "s-old", "r");
        db.execute(
            "UPDATE thread SET title='Child investigation' WHERE id='child'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('child-turn','child','child-run','running','2020-01-01T00:00:01Z')", []).unwrap();
        let value = json!({"operation_id":"w", "caller":{"run_id":"r","execution_id":"s-old"}, "accepted_at":"2020-01-01T00:00:02Z", "timeout_ms":30000,
            "conditions":[{"type":"turn_completed","turn_id":"child-turn","execution_id":"s-new"}]}).to_string();
        db.execute(
            "UPDATE app_meta SET value=?1 WHERE key='wait.pending.w'",
            [&value],
        )
        .unwrap();
        let snapshot =
            prepare(read_tx_limited(&db, "r", Some(&[("w".into(), "s-old".into())]), 10).unwrap());
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
        pending(&db, "w", "s-old", "r");
        db.execute("UPDATE thread SET title='First child' WHERE id='child'", [])
            .unwrap();
        let scope = [("w".into(), "s-old".into())];
        for (condition, expected) in [
            (
                json!({"type":"thread_settled", "thread_ids":["child","lineage"], "states":["completed"]}),
                "First child | 他1会話",
            ),
            (
                json!({"type":"run_ended", "execution_id":"s-new"}),
                "対象不明",
            ),
        ] {
            let value = json!({"operation_id":"w", "caller":{"run_id":"r","execution_id":"s-old"}, "accepted_at":"2020-01-01T00:00:02Z", "timeout_ms":30000, "conditions":[condition]}).to_string();
            db.execute(
                "UPDATE app_meta SET value=?1 WHERE key='wait.pending.w'",
                [&value],
            )
            .unwrap();
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
    fn invalid_or_huge_wait_timeout_is_unknown_and_never_fails_the_snapshot() {
        let db = fixture();
        pending(&db, "w", "s-old", "r");
        for (timeout, expected) in [
            (json!(0), Some(Duration::ZERO)),
            (json!(-1), None),
            (json!(u64::MAX), None),
        ] {
            let value = json!({"operation_id":"w", "caller":{"run_id":"r","execution_id":"s-old"}, "accepted_at":"2020-01-01T00:00:02Z", "timeout_ms":timeout, "conditions":[{"type":"shell_ended","execution_id":"s-new"}]}).to_string();
            db.execute(
                "UPDATE app_meta SET value=?1 WHERE key='wait.pending.w'",
                [&value],
            )
            .unwrap();
            let snapshot = prepare(
                read_tx_limited(&db, "r", Some(&[("w".into(), "s-old".into())]), 10).unwrap(),
            );
            let wait = snapshot
                .rows
                .iter()
                .find(|r| matches!(r.target, LiveWorkTarget::Wait { .. }))
                .unwrap();
            assert_eq!(wait.wait_info.as_ref().unwrap().timeout, expected);
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
        pending(&db, "wait-old", "s-old", "r");
        let scope = [("wait-old".into(), "s-old".into())];
        let trace = Statements::new(&db);
        let raw = read_tx(&db, "r", Some(&scope)).unwrap();
        assert_eq!(
            trace.count(),
            3,
            "one live batch plus two bounded untitled-child witnesses"
        );
        drop(trace);
        db.execute("UPDATE shell_execution SET state='exited'", [])
            .unwrap();
        db.execute("DELETE FROM app_meta", []).unwrap();
        drop(db);
        let snapshot = prepare(raw);
        assert_eq!(
            (
                snapshot.shells,
                snapshot.children,
                snapshot.waits,
                snapshot.overflow
            ),
            (4, 1, Some(1), 2)
        );
        assert_eq!(snapshot.rows.len(), 3);
        assert!(
            matches!(&snapshot.rows[0].target, LiveWorkTarget::ChildRun { run_id, .. } if run_id == "child-run")
        );
        assert!(
            matches!(&snapshot.rows[1].target, LiveWorkTarget::Wait { operation_id, .. } if operation_id == "wait-old")
        );
        assert!(
            matches!(&snapshot.rows[2].target, LiveWorkTarget::Shell { execution_id, .. } if execution_id == "s-new")
        );
        assert_eq!(snapshot.rows[0].label, "未送信");
    }

    #[test]
    fn exact_wait_scope_proves_owner_and_pending_key_without_os_handle() {
        let db = fixture();
        pending(&db, "valid", "s-old", "r");
        pending(&db, "wrong-caller", "s-old", "lineage-run");
        pending(&db, "foreign", "foreign", "r");
        pending(&db, "not-in-scope", "s-old", "r");
        let scope = vec![
            ("valid".into(), "s-old".into()),
            ("wrong-caller".into(), "s-old".into()),
            ("foreign".into(), "foreign".into()),
            ("expired".into(), "s-old".into()),
        ];
        let observed = prepare(read_tx(&db, "r", Some(&scope)).unwrap());
        assert_eq!(observed.waits, Some(1));
        assert_eq!(prepare(read_tx(&db, "r", None).unwrap()).waits, None);
        assert_eq!(
            prepare(read_tx(&db, "r", Some(&[])).unwrap()).waits,
            Some(0)
        );
        // Registry identity plus durable pending metadata, not an OS handle,
        // supplies the wait fact even when its source process already exited.
        db.execute(
            "UPDATE shell_execution SET state='exited' WHERE id='s-old'",
            [],
        )
        .unwrap();
        assert_eq!(
            prepare(read_tx(&db, "r", Some(&scope)).unwrap()).waits,
            Some(1)
        );
        let trace = Statements::new(&db);
        assert!(read_tx(&db, "r", Some(&vec![("a".into(), "b".into()); 129])).is_err());
        assert_eq!(trace.count(), 0);
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
        let child = prepare(raw)
            .rows
            .into_iter()
            .find(|row| matches!(row.target, LiveWorkTarget::ChildRun { .. }))
            .unwrap();
        assert_eq!(child.label, "Known child question");
        assert_eq!(
            child.state,
            crate::activity::ActivityPhase::Recovery.column_label()
        );
    }

    #[test]
    fn malformed_present_wait_proof_is_unknown_without_fabricated_wait_rows() {
        let db = fixture();
        db.execute(
            "INSERT INTO app_meta(key,value)VALUES('wait.pending.corrupt','not JSON')",
            [],
        )
        .unwrap();
        let scope = [("corrupt".into(), "s-old".into())];
        let snapshot = prepare(read_tx(&db, "r", Some(&scope)).unwrap());
        assert_eq!(snapshot.waits, None);
        assert!(
            snapshot
                .rows
                .iter()
                .all(|row| !matches!(row.target, LiveWorkTarget::Wait { .. }))
        );
        db.execute("UPDATE app_meta SET value='{\"operation_id\":\"corrupt\",\"caller\":{}}' WHERE key='wait.pending.corrupt'",[]).unwrap();
        assert_eq!(
            prepare(read_tx(&db, "r", Some(&scope)).unwrap()).waits,
            None
        );
        db.execute("DELETE FROM app_meta WHERE key='wait.pending.corrupt'", [])
            .unwrap();
        assert_eq!(
            prepare(read_tx(&db, "r", Some(&scope)).unwrap()).waits,
            Some(0)
        );
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
        assert!(
            matches!(&snapshot.rows[2].target,LiveWorkTarget::ChildRun{run_id,..}if run_id=="child-run")
        );
    }

    #[test]
    fn delayed_preparation_keeps_snapshot_clock_before_new_output_markers() {
        let db = fixture();
        let raw = read_tx(&db, "r", Some(&[])).unwrap();
        let observed_at = raw.observed_at;
        let expected_elapsed = age(Some("2020-01-01T00:00:00Z"), raw.wall_at);
        // A real commit marker can arrive while the finite presentation worker
        // is occupied. Its time must stay newer than this older DB snapshot.
        std::thread::sleep(Duration::from_millis(5));
        let newer_marker = Instant::now();
        std::thread::sleep(Duration::from_millis(5));
        let prepared = prepare(raw);
        assert_eq!(prepared.observed_at, observed_at);
        assert!(
            prepared.observed_at < newer_marker,
            "UI supersession must preserve a marker newer than the DB snapshot, even if preparation happens later"
        );
        assert_eq!(prepared.rows[0].elapsed_base, expected_elapsed);
        assert!(
            Instant::now().saturating_duration_since(prepared.observed_at)
                >= Duration::from_millis(10)
        );
    }
}
