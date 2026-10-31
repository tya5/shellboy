//! Shared owned saved-summary facts; titles and excerpts remain caller policy.
use crate::{protocol::*, storage::StorageError};
use rusqlite::{Connection, Row};

/// Aliases t/latest/u/a are shared by summary and bounded lineage readers.
pub(crate) const ACTIVITY_COLUMNS: &str = "latest.state,u.state,
    EXISTS(SELECT 1 FROM shell_execution WHERE owner_run_id=latest.id AND state IN ('starting','running','stopping'))
      OR EXISTS(SELECT 1 FROM run WHERE owner_run_id=latest.id AND state IN ('live','stopping')),
    a.state,CASE WHEN a.state='failed' AND a.generation=u.generation AND json_valid(a.error_json)
      THEN CASE WHEN octet_length(json_extract(a.error_json,'$.automatic_retry'))<=256 THEN json_extract(a.error_json,'$.automatic_retry') END END,
    CASE WHEN latest.state='live' AND u.state='running' AND COALESCE(a.state,'')<>'inflight'
      THEN EXISTS(SELECT 1 FROM tool_group WHERE turn_id=u.id AND state='running') ELSE 0 END";
pub(crate) const RUN_TURN_JOINS: &str = "LEFT JOIN run latest ON latest.id=(
    SELECT r.id FROM run r WHERE r.thread_id=t.id ORDER BY r.created_at DESC,r.id DESC LIMIT 1)
LEFT JOIN turn u ON u.id=(SELECT id FROM turn WHERE run_id=latest.id ORDER BY created_at DESC,id DESC LIMIT 1)";
fn activity_joins() -> String {
    format!(
        "{RUN_TURN_JOINS}
LEFT JOIN model_attempt a ON a.rowid=(SELECT rowid FROM model_attempt WHERE turn_id=u.id
    AND latest.state='live' AND u.state='running' ORDER BY rowid DESC LIMIT 1)"
    )
}
fn columns_from() -> String {
    let activity_joins = activity_joins();
    format!("SELECT t.id,t.title,t.cwd,t.parent_thread_id,t.updated_at,
    COALESCE(u.state,latest.state), latest.stop_reason,latest.id,{ACTIVITY_COLUMNS},
    CASE WHEN json_valid(revisit.value) THEN json_type(revisit.value)='object'
      AND json_type(revisit.value,'$.consumed_count')='integer'
      AND (json_type(revisit.value,'$.last_fired_at') IS NULL OR json_type(revisit.value,'$.last_fired_at')='null'
           OR (json_type(revisit.value,'$.last_fired_at')='text' AND length(json_extract(revisit.value,'$.last_fired_at'))<=128))
      AND (json_type(revisit.value,'$.pending') IS NULL OR json_type(revisit.value,'$.pending')='null'
           OR (json_type(revisit.value,'$.pending')='object' AND json_type(revisit.value,'$.pending.text')='text'
             AND json_type(revisit.value,'$.pending.operation_id')='text' AND json_type(revisit.value,'$.pending.set_at')='text'
             AND json_type(revisit.value,'$.pending.set_by') IS NOT NULL)) ELSE 0 END,
    CASE WHEN json_valid(revisit.value) THEN json_type(revisit.value,'$.pending')='object' ELSE 0 END,
    CASE WHEN json_valid(revisit.value) THEN substr(revisit.value->'$.consumed_count',1,21) END,
    CASE WHEN json_valid(revisit.value) THEN CASE WHEN json_type(revisit.value,'$.last_fired_at')='text' THEN substr(json_extract(revisit.value,'$.last_fired_at'),1,128) END END,
    u.id,u.answer_node_id,latest.ended_at,a.started_at,u.generation
FROM thread t {activity_joins}
LEFT JOIN app_meta revisit ON revisit.key='revisit.'||latest.id
")
}

#[derive(Clone)]
pub(crate) struct RawActivity {
    run: Option<String>,
    turn: Option<String>,
    owned: bool,
    attempt: Option<String>,
    retry: Option<String>,
    tools: bool,
}
pub(crate) fn decode_activity(row: &Row<'_>, offset: usize) -> rusqlite::Result<RawActivity> {
    Ok(RawActivity {
        run: row.get(offset)?,
        turn: row.get(offset + 1)?,
        owned: row.get(offset + 2)?,
        attempt: row.get(offset + 3)?,
        retry: row.get(offset + 4)?,
        tools: row.get(offset + 5)?,
    })
}
pub(crate) fn prepare_activity(raw: &RawActivity) -> crate::activity::ActivityPhase {
    let retrying = raw
        .retry
        .as_deref()
        .and_then(|body| {
            serde_json::from_str::<crate::ui::activity::AutomaticRetrySnapshot>(body).ok()
        })
        .is_some_and(|retry| retry.number > 0 && retry.number <= retry.limit);
    crate::activity::classify(
        raw.run.as_deref(),
        raw.turn.as_deref(),
        raw.owned,
        raw.attempt.as_deref() == Some("inflight"),
        retrying,
        raw.tools,
    )
}
/// Native-state category only: no JSON, presentation formatting or extra reads.
/// Inflight/retry/tool/ownership refinements do not alter Watch eligibility.
pub(crate) fn coarse_activity(raw: &RawActivity) -> crate::activity::ActivityPhase {
    crate::activity::classify(
        raw.run.as_deref(),
        raw.turn.as_deref(),
        false,
        false,
        false,
        false,
    )
}

#[derive(Clone)]
pub(crate) struct RawSummary {
    pub(crate) thread_id: String,
    pub(crate) stored_title: String,
    pub(super) cwd: String,
    pub(crate) parent_id: Option<String>,
    updated_at: String,
    state: Option<String>,
    stop_reason: Option<String>,
    pub(crate) activity: RawActivity,
    pub(crate) identity: SummaryIdentity,
    revisit_valid: bool,
    revisit_pending: bool,
    revisit_count: Option<String>,
    revisit_fired: Option<String>,
}

/// Saved identities/times, never an invented timestamp for a phase transition.
#[derive(Clone)]
pub(crate) struct SummaryIdentity {
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub answer_id: Option<String>,
    pub run_ended_at: Option<String>,
    pub attempt_started_at: Option<String>,
    pub turn_generation: Option<u64>,
}

pub(super) fn page_sql() -> String {
    let columns_from = columns_from();
    format!(
        "{columns_from}WHERE t.deleted_at IS NULL AND t.rowid<=?1
      AND (?2='' OR t.updated_at<?2 OR (t.updated_at=?2 AND t.id<?3))
      ORDER BY t.updated_at DESC,t.id DESC LIMIT ?4"
    )
}
pub(super) fn decode(row: &Row<'_>) -> rusqlite::Result<RawSummary> {
    Ok(RawSummary {
        thread_id: row.get(0)?,
        stored_title: row.get(1)?,
        cwd: row.get(2)?,
        parent_id: row.get(3)?,
        updated_at: row.get(4)?,
        state: row.get(5)?,
        stop_reason: row.get(6)?,
        activity: decode_activity(row, 8)?,
        identity: SummaryIdentity {
            run_id: row.get(7)?,
            turn_id: row.get(18)?,
            answer_id: row.get(19)?,
            run_ended_at: row.get(20)?,
            attempt_started_at: row.get(21)?,
            turn_generation: row
                .get::<_, Option<i64>>(22)?
                .and_then(|value| u64::try_from(value).ok()),
        },
        revisit_valid: row.get::<_, Option<bool>>(14)?.unwrap_or(false),
        revisit_pending: row.get::<_, Option<bool>>(15)?.unwrap_or(false),
        revisit_count: row.get(16)?,
        revisit_fired: row.get(17)?,
    })
}
pub(crate) fn prepare(raw: RawSummary, title: String, excerpt: String) -> ThreadSummary {
    ThreadSummary {
        activity: prepare_activity(&raw.activity),
        thread_id: raw.thread_id,
        title,
        cwd: raw.cwd,
        parent_id: raw.parent_id,
        state: raw.state.unwrap_or_else(|| "idle".into()),
        latest_run_stop_reason: raw.stop_reason,
        revisit: raw
            .revisit_count
            .as_deref()
            .filter(|_| raw.revisit_valid)
            .and_then(|count| count.parse::<u64>().ok())
            .map(|consumed_count| RevisitSummary {
                pending: raw.revisit_pending,
                consumed_count,
                last_fired_at: raw.revisit_fired,
            }),
        updated_at: raw.updated_at,
        excerpt,
    }
}
pub(crate) fn read_ids_tx(
    db: &Connection,
    ids: &[String],
) -> Result<Vec<RawSummary>, StorageError> {
    if ids.len() > 128 {
        return Err(
            ApiError::new(ErrorCode::InputLimit, "Thread summary scope exceeds128 IDs").into(),
        );
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql=columns_from().replace("FROM thread t", "FROM (SELECT DISTINCT value AS id FROM json_each(?1)) requested CROSS JOIN thread t ON t.id=requested.id");
    let sql = format!("{sql}WHERE t.deleted_at IS NULL");
    let encoded = serde_json::to_string(ids).expect("Thread IDs serialize");
    let rows = db
        .prepare(&sql)?
        .query_map([encoded], decode)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Same normalization policy for CLI filtering and browser patch membership.
pub(crate) fn matches_filter(thread_id: &str, title: &str, cwd: &str, query: &str) -> bool {
    let needle = super::title_text(query).to_lowercase();
    needle.is_empty()
        || super::title_text(&format!("{thread_id} {title} {cwd}"))
            .to_lowercase()
            .contains(&needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::sql_trace::Statements;
    use rusqlite::StatementStatus;
    use serde_json::json;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','Stored title','/work','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','recovery_wait','now');").unwrap();
        db
    }
    #[test]
    fn exact_id_batch_is_one_statement_with_saved_activity_and_bounded_revisit_proof() {
        let db = fixture();
        let state = json!({"pending":{"text":"x".repeat(8192),"set_by":null,"operation_id":"op","set_at":"now"},"consumed_count":u64::MAX,"last_fired_at":"earlier"});
        db.execute(
            "INSERT INTO app_meta VALUES('revisit.r',?1)",
            [state.to_string()],
        )
        .unwrap();
        let trace = Statements::new(&db);
        let rows = read_ids_tx(&db, &["t".into(), "missing".into(), "t".into()]).unwrap();
        assert_eq!(trace.count(), 1);
        drop(trace);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].identity.run_id.as_deref(), Some("r"));
        assert_eq!(rows[0].identity.turn_id.as_deref(), Some("u"));
        assert_eq!(rows[0].identity.turn_generation, Some(0));
        let summary = prepare(
            rows.into_iter().next().unwrap(),
            "Stored title".into(),
            String::new(),
        );
        assert_eq!(summary.activity, crate::activity::ActivityPhase::Recovery);
        assert_eq!(
            serde_json::to_value(summary.revisit).unwrap(),
            serde_json::to_value(crate::control::revisit::summary(&db, "r").unwrap()).unwrap()
        );
        for malformed in [
            "{}",
            "null",
            r#"{"pending":{},"consumed_count":1,"last_fired_at":null}"#,
        ] {
            db.execute(
                "UPDATE app_meta SET value=?1 WHERE key='revisit.r'",
                [malformed],
            )
            .unwrap();
            let raw = read_ids_tx(&db, &["t".into()]).unwrap().pop().unwrap();
            assert!(
                prepare(raw, "title".into(), String::new())
                    .revisit
                    .is_none()
            );
        }
    }
    #[test]
    fn requested_summary_plan_does_not_scale_with_unrelated_threads() {
        let mut db = fixture();
        let sql=format!("{}WHERE t.deleted_at IS NULL",columns_from().replace("FROM thread t","FROM (SELECT DISTINCT value AS id FROM json_each(?1)) requested CROSS JOIN thread t ON t.id=requested.id"));
        let measure = |db: &Connection| {
            let mut stmt = db.prepare(&sql).unwrap();
            let rows = stmt
                .query_map([r#"["t"]"#], decode)
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(rows.len(), 1);
            stmt.get_status(StatementStatus::VmStep)
        };
        let before = measure(&db);
        let tx = db.transaction().unwrap();
        for n in 0..10_000 {
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','','p','m','now')",[format!("other-{n}")]).unwrap();
        }
        tx.commit().unwrap();
        let after = measure(&db);
        assert!(after <= before + 100, "{before}->{after}");
        let trace = Statements::new(&db);
        assert!(read_ids_tx(&db, &[]).unwrap().is_empty());
        assert!(read_ids_tx(&db, &vec!["t".into(); 129]).is_err());
        assert_eq!(trace.count(), 0);
    }
    #[test]
    fn pure_filter_matches_existing_normalization() {
        assert!(matches_filter(
            "id",
            "HELLO\n world",
            "/work",
            "hello world"
        ));
        assert!(matches_filter("id", "title", "/Mixed/Path", "mixed/path"));
        assert!(!matches_filter("id", "title", "/work", "absent"));
    }
}
