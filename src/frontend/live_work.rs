//! Bounded observations of work owned by the selected saved Run.
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget},
};
use rusqlite::Connection;
use std::time::{Duration, Instant};

const MAX_WAITS: usize = 128;
// Counts are authoritative saved ownership counts. Only three rows of each
// process kind enter the final merge; wait candidates are the bounded exact
// registry keys, never a scan of all pending app_meta entries.
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
 SELECT operation_id,execution_id,
        CASE WHEN json_valid(value) THEN CASE WHEN json_type(value,'$.accepted_at')='text' THEN json_extract(value,'$.accepted_at') END END AS started_at
 FROM wait_observations
 WHERE CASE WHEN proof_valid THEN json_extract(value,'$.operation_id')=operation_id
     AND json_extract(value,'$.caller.run_id')=?1
     AND json_extract(value,'$.caller.execution_id')=execution_id ELSE 0 END
), shell_members AS MATERIALIZED (
 SELECT id,started_at FROM shell_execution
 WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')
 ORDER BY julianday(started_at) IS NULL,julianday(started_at),id LIMIT 3
), shells AS MATERIALIZED (
 SELECT 'shell' AS kind,s.id AS id,s.turn_id AS related,s.id AS execution,
        CASE WHEN json_valid(s.shell_json) THEN CASE WHEN json_type(s.shell_json,'$.command')='text' THEN substr(json_extract(s.shell_json,'$.command'),1,512) END END AS label,
        s.state,s.started_at,
        (SELECT received_at FROM output_chunk o WHERE o.execution_id=s.id ORDER BY o.sequence DESC LIMIT 1) AS output_at
 FROM shell_members member CROSS JOIN shell_execution s ON s.id=member.id
), children AS MATERIALIZED (
 SELECT 'child' AS kind,r.id AS id,r.thread_id AS related,NULL AS execution,
        substr(t.title,1,512) AS label,r.state,r.created_at AS started_at,NULL AS output_at
 FROM run r JOIN thread t ON t.id=r.thread_id
 WHERE r.owner_run_id=?1 AND r.state IN ('live','stopping')
 ORDER BY julianday(r.created_at) IS NULL,julianday(r.created_at),r.id LIMIT 3
), wait_rows AS MATERIALIZED (
 SELECT 'wait' AS kind,operation_id AS id,NULL AS related,execution_id AS execution,
        NULL AS label,'waiting' AS state,started_at,NULL AS output_at FROM waits
 ORDER BY julianday(started_at) IS NULL,julianday(started_at),operation_id LIMIT 3
), picked AS MATERIALIZED (
 SELECT * FROM (SELECT * FROM shells UNION ALL SELECT * FROM children UNION ALL SELECT * FROM wait_rows)
 ORDER BY julianday(started_at) IS NULL,julianday(started_at),id,kind LIMIT 3
), counts AS MATERIALIZED (
 SELECT (SELECT count(*) FROM shell_execution WHERE owner_run_id=?1 AND state IN ('starting','running','stopping')) AS shells,
        (SELECT count(*) FROM run WHERE owner_run_id=?1 AND state IN ('live','stopping')) AS children,
        (SELECT count(*) FROM waits) AS waits,
        (SELECT count(*) FROM wait_observations WHERE COALESCE(proof_valid,0)=0) AS invalid_waits
)
SELECT picked.kind,picked.id,picked.related,picked.execution,picked.label,picked.state,picked.started_at,picked.output_at,
       counts.shells,counts.children,counts.waits,counts.invalid_waits,
       child_turn.state,child_attempt.state,
       CASE WHEN child_attempt.state='failed' AND child_attempt.generation=child_turn.generation AND json_valid(child_attempt.error_json)
         THEN json_extract(child_attempt.error_json,'$.automatic_retry') END,
       CASE WHEN picked.kind='child' THEN
         EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=picked.id AND state IN ('starting','running','stopping'))
         OR EXISTS(SELECT 1 FROM run WHERE owner_run_id=picked.id AND state IN ('live','stopping')) ELSE 0 END,
       CASE WHEN picked.kind='child' AND picked.state='live' AND child_turn.state='running' AND COALESCE(child_attempt.state,'')<>'inflight'
         THEN EXISTS(SELECT 1 FROM tool_group WHERE turn_id=child_turn.id AND state='running') ELSE 0 END
FROM counts LEFT JOIN picked ON 1
LEFT JOIN turn child_turn ON child_turn.id=(SELECT id FROM turn WHERE picked.kind='child' AND run_id=picked.id ORDER BY created_at DESC,id DESC LIMIT 1)
LEFT JOIN model_attempt child_attempt ON child_attempt.rowid=(SELECT rowid FROM model_attempt WHERE turn_id=child_turn.id
    AND picked.state='live' AND child_turn.state='running' ORDER BY rowid DESC LIMIT 1)
ORDER BY julianday(picked.started_at) IS NULL,julianday(picked.started_at),picked.id,picked.kind";

pub(super) struct RawLiveWork {
    run_id: String,
    rows: Vec<RawRow>,
    shells: u64,
    children: u64,
    waits: Option<u64>,
    known_waits: u64,
    observed_at: Instant,
    wall_at: chrono::DateTime<chrono::Utc>,
}
struct RawRow {
    kind: String,
    id: String,
    related: Option<String>,
    execution: Option<String>,
    label: Option<String>,
    state: String,
    started_at: Option<String>,
    output_at: Option<String>,
    title: Option<super::header::TitleWitness>,
    child_phase: Option<ChildPhase>,
}
struct ChildPhase {
    turn: Option<String>,
    attempt: Option<String>,
    retry: Option<String>,
    owned: bool,
    tools: bool,
}

pub(super) fn read_tx(
    db: &Connection,
    run_id: &str,
    waits: Option<&[(String, String)]>,
) -> Result<RawLiveWork, StorageError> {
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
        observed_at: Instant::now(),
        wall_at: chrono::Utc::now(),
    };
    let mut statement = db.prepare(LIVE_SQL)?;
    let mut rows = statement.query(rusqlite::params![run_id, encoded])?;
    while let Some(row) = rows.next()? {
        result.shells = row.get::<_, i64>(8)? as u64;
        result.children = row.get::<_, i64>(9)? as u64;
        result.known_waits = row.get::<_, i64>(10)? as u64;
        result.waits = (known && row.get::<_, i64>(11)? == 0).then_some(result.known_waits);
        if let Some(kind) = row.get::<_, Option<String>>(0)? {
            result.rows.push(RawRow {
                kind,
                id: row.get(1)?,
                related: row.get(2)?,
                execution: row.get(3)?,
                label: row.get(4)?,
                state: row.get(5)?,
                started_at: row.get(6)?,
                output_at: row.get(7)?,
                title: None,
                child_phase: if row.get::<_, String>(0)? == "child" {
                    Some(ChildPhase {
                        turn: row.get(12)?,
                        attempt: row.get(13)?,
                        retry: row.get(14)?,
                        owned: row.get(15)?,
                        tools: row.get(16)?,
                    })
                } else {
                    None
                },
            });
        }
    }
    drop(rows);
    drop(statement);
    // Final display membership is already capped at3. Reuse the header's
    // finite evidence policy only for those children, inside this same read.
    for row in &mut result.rows {
        if row.kind == "child" {
            row.title = super::header::read_title_tx(
                db,
                row.related.as_deref().expect("saved child Thread"),
                row.label.as_deref().unwrap_or_default(),
            )?;
        }
    }
    // Capture after the last same-snapshot child/title observation and before
    // yielding the DB worker: later output commits must remain newer markers.
    result.observed_at = Instant::now();
    result.wall_at = chrono::Utc::now();
    Ok(result)
}

fn age(saved: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let saved = chrono::DateTime::parse_from_rfc3339(saved?).ok()?;
    now.signed_duration_since(saved).to_std().ok()
}
fn label(value: Option<&str>, fallback: &str) -> String {
    let (safe, _) = crate::presentation::sanitize_plain(value.unwrap_or_default(), 0);
    let safe: String = safe.chars().take(512).collect();
    if safe.trim().is_empty() {
        fallback.into()
    } else {
        safe
    }
}

pub(super) fn prepare(raw: RawLiveWork) -> LiveWorkSnapshot {
    let observed_at = raw.observed_at;
    let now = raw.wall_at;
    let overflow = raw
        .shells
        .saturating_add(raw.children)
        .saturating_add(raw.known_waits)
        .saturating_sub(raw.rows.len() as u64);
    let rows = raw
        .rows
        .into_iter()
        .map(|mut row| {
            if let Some(phase) = row.child_phase.take() {
                let retrying = phase
                    .retry
                    .as_deref()
                    .and_then(|body| {
                        serde_json::from_str::<crate::ui::activity::AutomaticRetrySnapshot>(body)
                            .ok()
                    })
                    .is_some_and(|retry| retry.number > 0 && retry.number <= retry.limit);
                row.state = crate::activity::classify(
                    Some(&row.state),
                    phase.turn.as_deref(),
                    phase.owned,
                    phase.attempt.as_deref() == Some("inflight"),
                    retrying,
                    phase.tools,
                )
                .column_label()
                .into();
            }
            if let Some(title) = row.title.take() {
                row.label = Some(title.prepare());
            }
            let (target, display) = match row.kind.as_str() {
                "shell" => (
                    LiveWorkTarget::Shell {
                        execution_id: row.id,
                        turn_id: row.related,
                    },
                    label(row.label.as_deref(), "コマンド（内容未記録）"),
                ),
                "child" => (
                    LiveWorkTarget::ChildRun {
                        run_id: row.id,
                        thread_id: row.related.expect("saved child Thread"),
                    },
                    label(row.label.as_deref(), "名称未設定"),
                ),
                _ => (
                    LiveWorkTarget::Wait {
                        operation_id: row.id,
                        execution_id: row.execution.expect("validated wait source"),
                    },
                    "待機".into(),
                ),
            };
            LiveWorkRow {
                target,
                label: display,
                state: row.state,
                elapsed_base: age(row.started_at.as_deref(), now),
                output_age_base: age(row.output_at.as_deref(), now),
                started_at: row.started_at,
                last_output_at: row.output_at,
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
            (4, 1, Some(1), 3)
        );
        assert_eq!(snapshot.rows.len(), 3);
        assert!(
            matches!(&snapshot.rows[0].target, LiveWorkTarget::Shell { execution_id, .. } if execution_id=="s-old")
        );
        assert!(
            matches!(&snapshot.rows[1].target, LiveWorkTarget::ChildRun { run_id, .. } if run_id=="child-run")
        );
        assert!(
            matches!(&snapshot.rows[2].target, LiveWorkTarget::Wait { operation_id, .. } if operation_id=="wait-old")
        );
        assert_eq!(
            snapshot.rows[0].last_output_at.as_deref(),
            Some("2020-01-01T00:00:09Z")
        );
        assert!(snapshot.rows[0].elapsed_base.unwrap().as_secs() > 100);
        assert!(snapshot.rows[0].output_age_base.unwrap().as_secs() > 100);
        assert!(!snapshot.rows[0].label.contains('\u{1b}'));
        assert_eq!(snapshot.rows[1].label, "未送信");
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
