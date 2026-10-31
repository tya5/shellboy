//! Finite browser observations inside the caller's existing read transaction.
use crate::{
    control::db::thread_summary,
    protocol::{ApiError, ErrorCode, ThreadSummary},
    storage::StorageError,
};
use rusqlite::Connection;
#[cfg(test)]
use std::collections::HashSet;

pub(super) struct RawPatch {
    rows: Vec<thread_summary::RawSummary>,
    titles: super::header::RawTitles,
    missing_ids: Vec<String>,
    selected_excerpt: Option<RawExcerpt>,
}
pub(super) struct PreparedPatch {
    pub rows: Vec<ThreadSummary>,
    pub missing_ids: Vec<String>,
    pub limited_title_ids: Vec<String>,
    pub selected_excerpt: Option<(String, Result<String, String>)>,
    pub watch_summaries: Vec<crate::ui::watch::WatchSummary>,
}
struct RawExcerpt {
    thread: String,
    available: bool,
    truncated: bool,
    rows: Vec<(Option<String>, bool)>,
}
const EXCERPT_SQL: &str = r"
WITH selected AS MATERIALIZED (SELECT id FROM thread WHERE id=?1 AND deleted_at IS NULL),
prefix AS MATERIALIZED (
 SELECT node_id,ordinal FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 129
), bounded AS MATERIALIZED (SELECT * FROM prefix ORDER BY ordinal DESC LIMIT 128),
witnesses AS MATERIALIZED (
 SELECT b.ordinal,n.id,CASE WHEN n.kind IN ('user','user_message','message','assistant','assistant_response') THEN octet_length(n.payload_json) END AS bytes
 FROM bounded b CROSS JOIN node n ON n.id=b.node_id
), budgeted AS MATERIALIZED (
 SELECT *,sum(CASE WHEN bytes<=16384 THEN bytes ELSE 0 END)OVER(ORDER BY ordinal DESC ROWS UNBOUNDED PRECEDING) AS used FROM witnesses
)
SELECT CASE WHEN bytes<=16384 AND used<=16384 THEN (SELECT payload_json FROM node WHERE id=budgeted.id) END,
       bytes IS NOT NULL AND (bytes>16384 OR used>16384),(SELECT count(*) FROM prefix)>128
FROM selected LEFT JOIN budgeted ON 1 ORDER BY ordinal DESC";

#[cfg(test)]
pub(super) fn read_tx(
    db: &Connection,
    ids: &[String],
    selected: Option<&str>,
) -> Result<RawPatch, StorageError> {
    if ids.len() > 128 {
        return Err(
            ApiError::new(ErrorCode::InputLimit, "Browser patch scope exceeds128 IDs").into(),
        );
    }
    let rows = thread_summary::read_ids_tx(db, ids)?;
    let found = rows
        .iter()
        .map(|row| row.thread_id.as_str())
        .collect::<HashSet<_>>();
    let mut missing_ids = ids
        .iter()
        .filter(|id| !found.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    missing_ids.sort();
    missing_ids.dedup();
    let requests = rows
        .iter()
        .map(|row| (row.thread_id.clone(), row.stored_title.clone()))
        .collect::<Vec<_>>();
    let titles = super::header::read_titles_tx(db, &requests)?;
    let selected_excerpt = selected.map(|id| read_excerpt(db, id)).transpose()?;
    Ok(RawPatch {
        rows,
        titles,
        missing_ids,
        selected_excerpt,
    })
}
impl RawPatch {
    pub(super) fn rows(&self) -> &[thread_summary::RawSummary] {
        &self.rows
    }
    pub(super) fn title_ids(&self) -> std::collections::BTreeSet<String> {
        self.rows.iter().map(|row| row.thread_id.clone()).collect()
    }
}
pub(super) fn read_shared_tx(
    db: &Connection,
    rows: Vec<thread_summary::RawSummary>,
    missing_ids: Vec<String>,
    selected: Option<&str>,
) -> Result<RawPatch, StorageError> {
    if rows.len() > 128 {
        return Err(ApiError::new(ErrorCode::InputLimit, "Browser patch exceeds128 rows").into());
    }
    let requests = rows
        .iter()
        .map(|row| (row.thread_id.clone(), row.stored_title.clone()))
        .collect::<Vec<_>>();
    Ok(RawPatch {
        rows,
        titles: super::header::read_titles_tx(db, &requests)?,
        missing_ids,
        selected_excerpt: selected.map(|id| read_excerpt(db, id)).transpose()?,
    })
}
fn read_excerpt(db: &Connection, id: &str) -> Result<RawExcerpt, StorageError> {
    let mut result = RawExcerpt {
        thread: id.into(),
        available: false,
        truncated: false,
        rows: Vec::new(),
    };
    let mut statement = db.prepare(EXCERPT_SQL)?;
    let mut rows = statement.query([id])?;
    while let Some(row) = rows.next()? {
        result.available = true;
        result.truncated |= row.get::<_, bool>(2)?;
        result.rows.push((row.get(0)?, row.get(1)?));
    }
    Ok(result)
}
fn prepare_excerpt(raw: RawExcerpt) -> (String, Result<String, String>) {
    let value = if !raw.available {
        Err("Selected Thread is unavailable".into())
    } else {
        let mut excerpt = None;
        for (body, omitted) in raw.rows {
            if omitted {
                excerpt = Some(Err(
                    "Selected preview exceeds its finite source allowance".into()
                ));
                break;
            }
            let Some(payload) =
                body.and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            else {
                continue;
            };
            let text = crate::history::conversation_excerpt(&payload, 512);
            if !text.trim().is_empty() {
                excerpt = Some(Ok(text));
                break;
            }
        }
        excerpt.unwrap_or_else(|| {
            if raw.truncated {
                Err("Selected preview is outside its finite membership prefix".into())
            } else {
                Ok(String::new())
            }
        })
    };
    (raw.thread, value)
}
#[cfg(test)]
pub(super) fn prepare(raw: RawPatch) -> PreparedPatch {
    prepare_with_watch(raw, false)
}
pub(super) fn prepare_with_watch(raw: RawPatch, include_watch: bool) -> PreparedPatch {
    let (mut titles, limited_title_ids) = raw.titles.prepare();
    let mut watch_summaries = Vec::new();
    let rows = raw
        .rows
        .into_iter()
        .map(|row| {
            let title = titles
                .remove(&row.thread_id)
                .expect("saved summary title witness");
            let identity = (row.identity.run_id.clone(), row.identity.answer_id.clone());
            let prepared = thread_summary::prepare(row, title, String::new());
            if include_watch {
                watch_summaries.push(
                    crate::ui::watch::WatchSummary::prepare(&prepared)
                        .with_identity(identity.0, identity.1),
                );
            }
            prepared
        })
        .collect();
    PreparedPatch {
        rows,
        missing_ids: raw.missing_ids,
        limited_title_ids,
        selected_excerpt: raw.selected_excerpt.map(prepare_excerpt),
        watch_summaries,
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
        db
    }
    fn thread(db: &Connection, id: &str, title: &str) {
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,'/','','p','m','now')",rusqlite::params![id,title]).unwrap();
    }
    fn source(db: &Connection, thread: &str, ordinal: i64, payload: Option<String>) {
        let id = format!("{thread}-{ordinal}");
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now',?2)",
            rusqlite::params![id, payload],
        )
        .unwrap();
        db.execute(
            "INSERT INTO thread_path VALUES(?1,?2,?3)",
            rusqlite::params![thread, ordinal, id],
        )
        .unwrap();
    }
    #[test]
    fn metadata_patch_has_fixed_sql_and_no_excerpt_copy_until_selected() {
        let db = fixture();
        thread(&db, "stored", "Explicit");
        thread(&db, "derived", "");
        source(
            &db,
            "derived",
            0,
            Some(json!({"role":"user","content":"Derived first question"}).to_string()),
        );
        let trace = Statements::new(&db);
        let raw = read_tx(
            &db,
            &["stored".into(), "derived".into(), "gone".into()],
            None,
        )
        .unwrap();
        assert_eq!(trace.count(), 2, "one summary and one set title statement");
        drop(trace);
        let patch = prepare(raw);
        assert_eq!(patch.missing_ids, vec!["gone"]);
        assert!(patch.selected_excerpt.is_none());
        assert!(patch.rows.iter().all(|row| row.excerpt.is_empty()));
        assert_eq!(
            patch
                .rows
                .iter()
                .find(|row| row.thread_id == "derived")
                .unwrap()
                .title,
            "Derived first question"
        );
        let selected = prepare(read_tx(&db, &["derived".into()], Some("derived")).unwrap());
        assert_eq!(
            selected.selected_excerpt.unwrap(),
            ("derived".into(), Ok("Derived first question".into()))
        );
        let page = crate::control::db::thread_list(
            &db,
            &crate::protocol::ThreadListArgs {
                query: None,
                cursor: None,
                limit: 50,
            },
        )
        .unwrap();
        for row in patch.rows {
            let original = page
                .items
                .iter()
                .find(|old| old.thread_id == row.thread_id)
                .unwrap();
            assert_eq!(row.title, original.title);
            assert_eq!(row.activity, original.activity);
            assert_eq!(row.state, original.state);
        }
    }
    #[test]
    fn shared_title_body_budget_is_finite_and_d1_null_fallback_is_preserved() {
        let db = fixture();
        let mut ids = Vec::new();
        for n in 0..8 {
            let id = format!("t{n}");
            thread(&db, &id, "");
            source(
                &db,
                &id,
                0,
                Some(json!({"role":"user","content":"x".repeat(100_000)}).to_string()),
            );
            ids.push(id);
        }
        let patch = prepare(read_tx(&db, &ids, None).unwrap());
        assert!(patch.limited_title_ids.len() >= 3);
        assert!(patch.rows.iter().all(|row| !row.title.is_empty()));
        thread(&db, "null", "");
        source(&db, "null", 0, None);
        // Current schema rejects malformed JSON at insertion. A valid saved
        // user payload without readable text exercises the same fallback.
        source(
            &db,
            "null",
            1,
            Some(json!({"role":"user","content":42}).to_string()),
        );
        source(
            &db,
            "null",
            2,
            Some(json!({"role":"user","content":"First readable question"}).to_string()),
        );
        assert_eq!(
            prepare(read_tx(&db, &["null".into()], None).unwrap()).rows[0].title,
            "First readable question"
        );
    }
    #[test]
    fn selected_excerpt_oversize_or_missing_is_explicitly_unavailable() {
        let db = fixture();
        thread(&db, "large", "explicit");
        source(
            &db,
            "large",
            0,
            Some(json!({"role":"user","content":"x".repeat(32_000)}).to_string()),
        );
        let patch = prepare(read_tx(&db, &["large".into()], Some("large")).unwrap());
        assert!(patch.selected_excerpt.unwrap().1.is_err());
        assert!(
            prepare(read_tx(&db, &[], Some("missing")).unwrap())
                .selected_excerpt
                .unwrap()
                .1
                .is_err()
        );
        let trace = Statements::new(&db);
        assert!(read_tx(&db, &vec!["x".into(); 129], None).is_err());
        assert_eq!(trace.count(), 0);
    }
}
