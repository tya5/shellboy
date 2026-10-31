//! Watch discovery pages: finite retained witnesses on a read-only WAL snapshot.
use crate::{
    control::{Control, db::thread_summary},
    protocol::{ApiError, ErrorCode, Page, ThreadSummary},
    ui::{
        ThreadDisplayTime,
        watch::{CompletionIdentity, WatchSummary},
    },
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const METADATA_LIMIT: usize = 4096;
const WITNESS_LIMIT: usize = 128;
#[derive(Clone, Debug)]
pub(super) struct Request {
    pub filter: String,
    pub cursor: Option<String>,
    pub descendant_root: Option<String>,
    pub seen_completions: Vec<CompletionIdentity>,
    pub seen_history_limited: bool,
    pub limit: usize,
}
pub(super) struct PreparedPage {
    pub page: Page<ThreadSummary>,
    pub watch_summaries: Vec<WatchSummary>,
    pub display_times: Vec<ThreadDisplayTime>,
    pub read_sequence: u64,
    pub limited_titles: Vec<String>,
    pub scope: Option<super::lineage::DescendantSnapshot>,
    pub read_at: String,
    pub continuation_limited: bool,
    pub notice: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SortKey {
    priority: u8,
    updated_at: String,
    id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    mode: String,
    root: Option<String>,
    filter: String,
    seen_hash: String,
    watermark: i64,
    last: SortKey,
}
fn storage(error: impl Into<crate::storage::StorageError>) -> ApiError {
    super::snapshot::storage_error(error.into())
}
fn expired() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "Watch cursor no longer matches this scope",
    )
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InputLimit, message)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
}
fn seen(request: &Request) -> Result<(String, String), ApiError> {
    if request.limit == 0
        || request.limit > 50
        || request.filter.chars().count() > 256
        || request.seen_completions.len() > 2000
        || request
            .descendant_root
            .as_deref()
            .is_some_and(|root| !valid_id(root))
        || request
            .seen_completions
            .iter()
            .any(|key| !valid_id(&key.run_id) || !valid_id(&key.answer_id))
    {
        return Err(invalid("Watch page exceeds its bounded request scope"));
    }
    let keys = request
        .seen_completions
        .iter()
        .map(|key| (&key.run_id, &key.answer_id))
        .collect::<BTreeSet<_>>();
    let json = serde_json::to_string(&keys).expect("completion identities serialize");
    let hash = crate::digest::sha256_hex(
        serde_json::to_vec(&(&json, request.seen_history_limited)).expect("seen scope serializes"),
    );
    Ok((json, hash))
}
fn decode(request: &Request, seen_hash: &str) -> Result<Option<Cursor>, ApiError> {
    let Some(token) = &request.cursor else {
        return Ok(None);
    };
    if token.len() > 8192 {
        return Err(expired());
    }
    let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| expired())?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| expired())?;
    if cursor.version != 1
        || cursor.mode != "watch"
        || cursor.root != request.descendant_root
        || cursor.filter != request.filter
        || cursor.seen_hash != seen_hash
        || cursor.watermark < 0
        || cursor.last.priority > 5
        || !valid_id(&cursor.last.id)
        || cursor.last.updated_at.len() > 128
    {
        return Err(expired());
    }
    Ok(Some(cursor))
}
fn encode(
    request: &Request,
    seen_hash: &str,
    watermark: i64,
    last: SortKey,
) -> Result<String, ApiError> {
    let cursor = Cursor {
        version: 1,
        mode: "watch".into(),
        root: request.descendant_root.clone(),
        filter: request.filter.clone(),
        seen_hash: seen_hash.into(),
        watermark,
        last,
    };
    serde_json::to_vec(&cursor)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| expired())
}
struct Candidate {
    key: SortKey,
}
fn candidate_sql(scoped: bool) -> String {
    let from = if scoped {
        "FROM json_each(?7) scope CROSS JOIN thread t ON t.id=scope.value"
    } else {
        "FROM thread t"
    };
    // These coarse groups are exactly the Watch categories. Inflight/retry/tools
    // refine active labels but do not change candidate priority or membership.
    format!(
        r"WITH seen AS MATERIALIZED (
       SELECT json_extract(value,'$[0]') AS run_id,json_extract(value,'$[1]') AS answer_id FROM json_each(?2)
    ),facts AS MATERIALIZED (
       SELECT t.id,t.title,t.cwd,t.updated_at,latest.id AS run_id,latest.state AS run_state,u.state AS turn_state,u.answer_node_id
       {from} {} WHERE t.deleted_at IS NULL AND t.rowid<=?1
    ),ranked AS MATERIALIZED (
       SELECT *,CASE
         WHEN run_state IS NULL OR run_state='stopped' THEN NULL
         WHEN run_state='failed' THEN 1
         WHEN run_state='completed' THEN CASE WHEN answer_node_id IS NOT NULL AND answer_node_id<>'' THEN CASE WHEN ?3 THEN 5 ELSE 2 END END
         WHEN run_state='stopping' THEN 3
         WHEN turn_state='recovery_wait' THEN 0
         WHEN turn_state='failed' THEN 1
         WHEN turn_state='stopped' THEN NULL
         WHEN turn_state IN ('preparing','running','waiting') THEN 3
         ELSE 4 END AS priority FROM facts
    ) SELECT id,title,cwd,updated_at,priority FROM ranked
    WHERE priority IS NOT NULL AND NOT(priority IN (2,5) AND EXISTS(SELECT 1 FROM seen WHERE seen.run_id=ranked.run_id AND seen.answer_id=ranked.answer_node_id))
      AND (?4<0 OR priority>?4 OR(priority=?4 AND(updated_at<?5 OR(updated_at=?5 AND id<?6))))
    ORDER BY priority,updated_at DESC,id DESC LIMIT ?8",
        thread_summary::RUN_TURN_JOINS
    )
}

/// Assign sequence at request admission, before this independently scheduled RO
/// snapshot. A slow old page can never mint a revision newer than later patches.
pub(super) async fn read(
    control: &Control,
    request: Request,
    stop: tokio_util::sync::CancellationToken,
    progress_slots: std::sync::Arc<tokio::sync::Semaphore>,
    diagnostics: crate::diagnostics::Diagnostics,
) -> Result<PreparedPage, ApiError> {
    let _ = seen(&request)?;
    let sequence = super::message_queue::read_revision();
    let _permit = tokio::select! { biased;
        _ = stop.cancelled() => return Err(ApiError::new(ErrorCode::InvalidState, "Watch page observation closed")),
        permit = progress_slots.acquire_owned() => permit.map_err(|_| ApiError::new(ErrorCode::InvalidState, "Watch page admission is closed"))?,
    };
    let path = control.store.database.path().to_path_buf();
    super::presentation_work(move || {
        if stop.is_cancelled() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Watch page observation closed",
            ));
        }
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(storage)?;
        db.busy_timeout(std::time::Duration::from_millis(250))
            .and_then(|_| db.pragma_update(None, "query_only", true))
            .map_err(storage)?;
        let page = collect_observed(&db, &request, sequence, Some(&diagnostics))?;
        if stop.is_cancelled() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Watch page observation closed",
            ));
        }
        Ok(page)
    })
    .await?
}
#[cfg(test)]
fn collect(db: &Connection, request: &Request, sequence: u64) -> Result<PreparedPage, ApiError> {
    collect_observed(db, request, sequence, None)
}
fn collect_observed(
    db: &Connection,
    request: &Request,
    sequence: u64,
    diagnostics: Option<&crate::diagnostics::Diagnostics>,
) -> Result<PreparedPage, ApiError> {
    let (seen_json, seen_hash) = seen(request)?;
    let cursor = decode(request, &seen_hash)?;
    let tx = db.unchecked_transaction().map_err(storage)?;
    let watermark = match &cursor {
        Some(cursor) => cursor.watermark,
        None => tx
            .query_row("SELECT COALESCE(max(rowid),0) FROM thread", [], |row| {
                row.get(0)
            })
            .map_err(storage)?,
    };
    let read_at = chrono::Utc::now().to_rfc3339();
    let scope = if let Some(root) = &request.descendant_root {
        let began = std::time::Instant::now();
        let raw =
            super::lineage::read_roots_tx(&tx, std::slice::from_ref(root)).map_err(storage)?;
        if let Some(diagnostics) = diagnostics {
            diagnostics.record_lineage_query(
                crate::diagnostics::DiagnosticIds::default(),
                began.elapsed(),
                1,
                raw.iter().map(super::lineage::RawLineage::witnesses).sum(),
                raw.iter().any(super::lineage::RawLineage::truncated),
            );
        }
        raw.into_iter().next().map(super::lineage::prepare)
    } else {
        None
    };
    if scope.as_ref().is_some_and(|scope| !scope.available) {
        tx.commit().map_err(storage)?;
        return Ok(PreparedPage {
            page: Page {
                items: Vec::new(),
                next_cursor: None,
            },
            watch_summaries: Vec::new(),
            display_times: Vec::new(),
            read_sequence: sequence,
            limited_titles: Vec::new(),
            scope,
            read_at,
            continuation_limited: false,
            notice: Some("子孫の範囲を取得できませんでした".into()),
        });
    }
    let scoped_ids = serde_json::to_string(
        &scope
            .as_ref()
            .map(|scope| scope.threads.as_slice())
            .unwrap_or_default(),
    )
    .expect("lineage IDs serialize");
    let sql = candidate_sql(scope.is_some());
    let mut statement = tx.prepare(&sql).map_err(storage)?;
    let last = cursor.as_ref().map(|cursor| &cursor.last);
    let mut rows = statement
        .query(rusqlite::params![
            watermark,
            seen_json,
            request.seen_history_limited,
            last.map_or(-1, |key| i64::from(key.priority)),
            last.map_or("", |key| key.updated_at.as_str()),
            last.map_or("", |key| key.id.as_str()),
            scoped_ids,
            (METADATA_LIMIT + 1) as i64
        ])
        .map_err(storage)?;
    let mut candidates = Vec::new();
    let mut last_scanned = None;
    let mut scanned = 0;
    let mut limited = false;
    while let Some(row) = rows.next().map_err(storage)? {
        if scanned == METADATA_LIMIT {
            limited = true;
            break;
        }
        let id: String = row.get(0).map_err(storage)?;
        let stored: String = row.get(1).map_err(storage)?;
        let cwd: String = row.get(2).map_err(storage)?;
        let key = SortKey {
            id,
            updated_at: row.get(3).map_err(storage)?,
            priority: row.get(4).map_err(storage)?,
        };
        scanned += 1;
        last_scanned = Some(key.clone());
        if !stored.trim().is_empty()
            && !thread_summary::matches_filter(&key.id, &stored, &cwd, &request.filter)
        {
            continue;
        }
        // Matching explicit rows and unresolved derived titles share one128-ID
        // allowance and exactly one title witness/body budget for this page.
        candidates.push(Candidate { key });
        if candidates.len() == WITNESS_LIMIT {
            limited = true;
            break;
        }
    }
    drop(rows);
    drop(statement);
    let ids = candidates
        .iter()
        .map(|candidate| candidate.key.id.clone())
        .collect::<Vec<_>>();
    let raw = thread_summary::read_ids_tx(&tx, &ids).map_err(storage)?;
    let mut raw = raw
        .into_iter()
        .map(|row| (row.thread_id.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let requests = ids
        .iter()
        .filter_map(|id| {
            raw.get(id)
                .map(|row| (id.clone(), row.stored_title.clone()))
        })
        .collect::<Vec<_>>();
    let titles = super::header::read_titles_tx(&tx, &requests).map_err(storage)?;
    tx.commit().map_err(storage)?;
    // No persistence-worker body parsing. All witnesses were bounded before
    // copying, and the RO transaction is released before title preparation.
    let (mut titles, limited_titles) = titles.prepare();
    let limited_set = limited_titles
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut items = Vec::new();
    let mut summaries = Vec::new();
    let mut evaluated = None;
    let mut more = false;
    let mut output_limited = Vec::new();
    for candidate in candidates {
        let Some(row) = raw.remove(&candidate.key.id) else {
            evaluated = Some(candidate.key);
            continue;
        };
        let title = titles
            .remove(&row.thread_id)
            .expect("candidate title witness");
        let identity = (row.identity.run_id.clone(), row.identity.answer_id.clone());
        let summary = thread_summary::prepare(row, title, String::new());
        let uncertain = limited_set.contains(summary.thread_id.as_str());
        if !uncertain
            && !thread_summary::matches_filter(
                &summary.thread_id,
                &summary.title,
                &summary.cwd,
                &request.filter,
            )
        {
            evaluated = Some(candidate.key);
            continue;
        }
        if items.len() == request.limit {
            more = true;
            break;
        }
        if uncertain {
            output_limited.push(summary.thread_id.clone());
        }
        summaries.push(WatchSummary::prepare(&summary).with_identity(identity.0, identity.1));
        items.push(summary);
        evaluated = Some(candidate.key);
    }
    if !more {
        evaluated = last_scanned.or(evaluated);
    }
    let continuation = more || limited;
    let next_cursor = if continuation {
        evaluated
            .map(|key| encode(request, &seen_hash, watermark, key))
            .transpose()?
    } else {
        None
    };
    let continuation_limited = limited && !more;
    let display_times = super::thread_display_times(&items);
    Ok(PreparedPage {
        page: Page { items, next_cursor },
        watch_summaries: summaries,
        display_times,
        read_sequence: sequence,
        limited_titles: output_limited,
        scope,
        read_at,
        continuation_limited,
        notice: continuation_limited.then(|| "取得範囲の続きをPageDownで確認できます".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::ActivityPhase;
    use rusqlite::params;
    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db
    }
    fn request() -> Request {
        Request {
            filter: String::new(),
            cursor: None,
            descendant_root: None,
            seen_completions: Vec::new(),
            seen_history_limited: false,
            limit: 50,
        }
    }
    fn thread(db: &Connection, id: &str, parent: Option<&str>, title: &str, updated: &str) {
        db.execute("INSERT INTO thread(id,parent_thread_id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,?3,'/work','','p','m',?4)",params![id,parent,title,updated]).unwrap();
    }
    fn run(db: &Connection, id: &str, state: &str, turn_state: &str, answer: bool) {
        let run = format!("run-{id}");
        let turn = format!("turn-{id}");
        let node = format!("answer-{id}");
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,?3,'2026-01-01T00:00:00Z')",
            params![run, id, state],
        )
        .unwrap();
        if answer {
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'assistant_response','now','{\"text\":\"saved\"}')",[&node]).unwrap();
        }
        db.execute("INSERT INTO turn(id,thread_id,run_id,state,answer_node_id,created_at)VALUES(?1,?2,?3,?4,?5,'2026-01-01T00:00:00Z')",params![turn,id,run,turn_state,answer.then_some(node)]).unwrap();
    }
    fn add(db: &Connection, id: &str, state: &str, turn: &str, answer: bool) {
        thread(db, id, None, id, "now");
        run(db, id, state, turn, answer);
    }
    #[test]
    fn global_watch_discovers_old_active_rows_outside_first_list_page_and_prioritizes_states() {
        let mut db = fixture();
        let tx = db.transaction().unwrap();
        for n in 0..80 {
            thread(&tx, &format!("idle-{n}"), None, "Idle", "2099");
        }
        tx.commit().unwrap();
        add(&db, "recovery", "live", "recovery_wait", false);
        add(&db, "failed", "failed", "failed", false);
        add(&db, "answer", "completed", "completed", true);
        add(&db, "active", "live", "running", false);
        add(&db, "background", "live", "completed", true);
        add(&db, "stopped", "stopped", "stopped", true);
        add(&db, "no-answer", "completed", "completed", false);
        let result = collect(&db, &request(), 7).unwrap();
        assert_eq!(
            result
                .page
                .items
                .iter()
                .map(|row| row.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["recovery", "failed", "answer", "active", "background"]
        );
        assert_eq!(result.read_sequence, 7);
        assert!(result.page.next_cursor.is_none());
        assert_eq!(
            result.watch_summaries[2].latest_run_id.as_deref(),
            Some("run-answer")
        );
        assert_eq!(
            result.watch_summaries[2].answer_id.as_deref(),
            Some("answer-answer")
        );
        assert!(result.page.items.iter().all(|row| row.excerpt.is_empty()));
        assert_eq!(result.page.items[3].activity, ActivityPhase::Running);
    }
    #[test]
    fn exact_seen_pairs_and_overflow_uncertainty_bind_the_private_cursor() {
        let db = fixture();
        add(&db, "a", "completed", "completed", true);
        add(&db, "b", "completed", "completed", true);
        add(&db, "working", "live", "running", false);
        let mut query = request();
        query.seen_completions.push(CompletionIdentity {
            run_id: "run-a".into(),
            answer_id: "answer-a".into(),
        });
        query.seen_history_limited = true;
        query.limit = 1;
        let first = collect(&db, &query, 1).unwrap();
        assert_eq!(first.page.items[0].thread_id, "working");
        let cursor = first.page.next_cursor.unwrap();
        query.cursor = Some(cursor.clone());
        let next = collect(&db, &query, 2).unwrap();
        assert_eq!(next.page.items[0].thread_id, "b");
        assert!(next.page.next_cursor.is_none());
        query.seen_completions[0].answer_id = "different-answer".into();
        assert_eq!(
            collect(&db, &query, 3).err().unwrap().code,
            ErrorCode::CursorExpired
        );
        query.seen_completions[0].answer_id = "answer-a".into();
        query.cursor = Some(cursor);
        query.filter = "changed".into();
        assert_eq!(
            collect(&db, &query, 3).err().unwrap().code,
            ErrorCode::CursorExpired
        );
    }
    #[test]
    fn prefetched_witnesses_do_not_skip_the_fifty_first_match() {
        let mut db = fixture();
        let tx = db.transaction().unwrap();
        for n in 0..120 {
            let id = format!("working-{n:03}");
            thread(&tx, &id, None, "Match", "now");
            run(&tx, &id, "live", "running", false);
        }
        tx.commit().unwrap();
        let mut query = request();
        let first = collect(&db, &query, 1).unwrap();
        assert_eq!(first.page.items.len(), 50);
        assert_eq!(first.page.items[0].thread_id, "working-119");
        assert_eq!(first.page.items[49].thread_id, "working-070");
        query.cursor = first.page.next_cursor;
        let second = collect(&db, &query, 2).unwrap();
        assert_eq!(second.page.items.len(), 50);
        assert_eq!(second.page.items[0].thread_id, "working-069");
        query.cursor = second.page.next_cursor;
        let third = collect(&db, &query, 3).unwrap();
        assert_eq!(third.page.items.len(), 20);
        assert_eq!(third.page.items[0].thread_id, "working-019");
        assert!(third.page.next_cursor.is_none());
    }
    #[test]
    fn unicode_derived_titles_and_unknown_evidence_are_not_false_filter_misses() {
        let db = fixture();
        add(&db, "derived", "live", "running", false);
        db.execute("UPDATE thread SET title='' WHERE id='derived'", [])
            .unwrap();
        db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('question','user','now','{\"role\":\"user\",\"content\":\"日本語の質問\"}');INSERT INTO thread_path VALUES('derived',0,'question');").unwrap();
        let mut query = request();
        query.filter = "日本語".into();
        let result = collect(&db, &query, 1).unwrap();
        assert_eq!(result.page.items.len(), 1);
        assert_eq!(result.page.items[0].title, "日本語の質問");
        db.execute(
            "UPDATE node SET payload_json=?1 WHERE id='question'",
            [serde_json::json!({"role":"user","content":"x".repeat(200_000)}).to_string()],
        )
        .unwrap();
        let limited = collect(&db, &query, 2).unwrap();
        assert_eq!(limited.page.items.len(), 1);
        assert_eq!(limited.limited_titles, vec!["derived"]);
    }
    #[test]
    fn scoped_watch_traverses_deleted_bridge_excludes_root_and_reports_unavailability() {
        let db = fixture();
        thread(&db, "root", None, "Root", "now");
        run(&db, "root", "live", "running", false);
        thread(&db, "bridge", Some("root"), "Bridge", "now");
        thread(&db, "grandchild", Some("bridge"), "Grandchild", "now");
        run(&db, "grandchild", "live", "running", false);
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='bridge'", [])
            .unwrap();
        add(&db, "unrelated", "live", "running", false);
        let mut query = request();
        query.descendant_root = Some("root".into());
        let page = collect(&db, &query, 1).unwrap();
        assert_eq!(
            page.page
                .items
                .iter()
                .map(|row| row.thread_id.as_str())
                .collect::<Vec<_>>(),
            vec!["grandchild"]
        );
        assert_eq!(page.scope.unwrap().active, 1);
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='root'", [])
            .unwrap();
        let absent = collect(&db, &query, 2).unwrap();
        assert!(absent.page.items.is_empty());
        assert!(!absent.scope.unwrap().available);
        assert!(absent.notice.is_some());
    }
    #[test]
    fn witness_cap_returns_an_honest_continuation_even_without_filter_matches() {
        let mut db = fixture();
        let tx = db.transaction().unwrap();
        for n in 0..129 {
            let id = format!("untitled-{n:03}");
            thread(&tx, &id, None, "", "now");
            run(&tx, &id, "live", "running", false);
        }
        tx.commit().unwrap();
        let mut query = request();
        query.filter = "absent".into();
        let page = collect(&db, &query, 1).unwrap();
        assert!(page.page.items.is_empty());
        assert!(page.continuation_limited && page.notice.is_some());
        assert!(page.page.next_cursor.is_some());
        query.cursor = page.page.next_cursor;
        let tail = collect(&db, &query, 2).unwrap();
        assert!(tail.page.items.is_empty());
        assert!(tail.page.next_cursor.is_none());
        assert!(!tail.continuation_limited);
    }
    #[test]
    fn metadata_scan_cap_is_continuation_not_an_empty_exhaustive_page() {
        let mut db = fixture();
        let tx = db.transaction().unwrap();
        for n in 0..4097 {
            let id = format!("active-{n:04}");
            thread(&tx, &id, None, "Known nonmatch", "now");
            run(&tx, &id, "live", "running", false);
        }
        tx.commit().unwrap();
        let mut query = request();
        query.filter = "absent".into();
        let first = collect(&db, &query, 1).unwrap();
        assert!(first.page.items.is_empty());
        assert!(first.continuation_limited);
        query.cursor = first.page.next_cursor;
        assert!(query.cursor.is_some());
        let second = collect(&db, &query, 2).unwrap();
        assert!(second.page.items.is_empty());
        assert!(!second.continuation_limited);
        assert!(second.page.next_cursor.is_none());
    }
    #[test]
    fn candidate_priority_matches_native_classifier_categories() {
        let db = fixture();
        add(&db, "t", "live", "running", true);
        let sql = candidate_sql(false);
        for run in ["live", "stopping", "completed", "stopped", "failed"] {
            for turn in [
                "preparing",
                "running",
                "waiting",
                "recovery_wait",
                "completed",
                "stopped",
                "failed",
            ] {
                db.execute("UPDATE run SET state=?1 WHERE id='run-t'", [run])
                    .unwrap();
                db.execute("UPDATE turn SET state=?1 WHERE id='turn-t'", [turn])
                    .unwrap();
                let priority = db
                    .prepare(&sql)
                    .unwrap()
                    .query_map(
                        rusqlite::params![1000, "[]", false, -1, "", "", "[]", 4097],
                        |r| r.get::<_, u8>(4),
                    )
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                let expected = match crate::activity::classify(
                    Some(run),
                    Some(turn),
                    false,
                    false,
                    false,
                    false,
                ) {
                    ActivityPhase::Recovery => Some(0),
                    ActivityPhase::Failed => Some(1),
                    ActivityPhase::Completed => Some(2),
                    ActivityPhase::Idle | ActivityPhase::Stopped => None,
                    ActivityPhase::Background | ActivityPhase::Continuing => Some(4),
                    _ => Some(3),
                };
                assert_eq!(
                    priority,
                    expected.into_iter().collect::<Vec<_>>(),
                    "run={run} turn={turn}"
                );
            }
        }
    }
    #[test]
    fn cursor_watermark_excludes_new_threads_and_rejects_another_lineage_scope() {
        let db = fixture();
        add(&db, "a", "live", "running", false);
        add(&db, "b", "live", "running", false);
        let mut query = request();
        query.limit = 1;
        let first = collect(&db, &query, 1).unwrap();
        query.cursor = first.page.next_cursor;
        add(&db, "new-recovery", "live", "recovery_wait", false);
        let second = collect(&db, &query, 2).unwrap();
        assert_eq!(second.page.items[0].thread_id, "a");
        assert!(second.page.next_cursor.is_none());
        query.descendant_root = Some("a".into());
        assert_eq!(
            collect(&db, &query, 3).err().unwrap().code,
            ErrorCode::CursorExpired
        );
        query.cursor = None;
        query.limit = 0;
        assert_eq!(
            collect(&db, &query, 4).err().unwrap().code,
            ErrorCode::InputLimit
        );
    }
}
