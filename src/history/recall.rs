//! Bounded selected-path input recall.
use crate::{protocol::*, storage::StorageError};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) fn read(
    db: &Connection,
    args: &HistoryRecallArgs,
    limit: u64,
) -> Result<Result<HistoryRecallResult, ApiError>, StorageError> {
    let thread = &args.thread_id;
    let older = args.older;
    let before = args.anchor.as_ref().map(|anchor| anchor.ordinal);
    let before_node_id = args.anchor.as_ref().map(|anchor| anchor.node_id.as_str());
    let revision: Option<i64> = db
        .query_row(
            "SELECT selection_revision FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [thread],
            |row| row.get(0),
        )
        .optional()?;
    let Some(revision) = revision else {
        return Ok(Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            "Input history Thread is no longer available",
        )));
    };
    let revision = u64::try_from(revision)
        .map_err(|_| StorageError::Schema("Negative selected path revision".into()))?;
    if args
        .expected_revision
        .is_some_and(|expected| expected != revision)
    {
        return Ok(Err(ApiError::new(
            ErrorCode::CursorExpired,
            "Input history path changed; retry from the latest input",
        )));
    }
    let boundary = match before.map(i64::try_from).transpose() {
        Ok(boundary) => boundary,
        Err(_) => {
            return Ok(Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Input history ordinal is out of range",
            )));
        }
    };
    // The viewport revision may be deliberately old while reading pinned
    // history, and every appended node increments it. Instead bind this
    // independent cursor to the exact selected node: appends remain valid,
    // while rewind/replacement cannot reinterpret an ordinal as another input.
    if let Some(node_id) = before_node_id {
        let selected: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND ordinal=?2 AND node_id=?3)",
            params![thread, boundary, node_id],
            |row| row.get(0),
        )?;
        if !selected {
            return Ok(Err(ApiError::new(
                ErrorCode::CursorExpired,
                "Input history path changed; retry from the latest input",
            )));
        }
    }
    if !older && before.is_none() {
        return Ok(Ok(HistoryRecallResult::End));
    }
    // Seek by the indexed ordinal, then check only the admitted membership rows.
    let sql = if older {
        "WITH scope AS MATERIALIZED (SELECT ordinal,node_id FROM thread_path WHERE thread_id=?1 AND ordinal<?2 ORDER BY ordinal DESC LIMIT ?3) SELECT p.ordinal,CASE WHEN octet_length(p.node_id)<=512 THEN p.node_id END, n.kind IN ('user','user_message') AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL FROM scope p JOIN node n ON n.id=p.node_id ORDER BY p.ordinal DESC"
    } else {
        "WITH scope AS MATERIALIZED (SELECT ordinal,node_id FROM thread_path WHERE thread_id=?1 AND ordinal>?2 ORDER BY ordinal ASC LIMIT ?3) SELECT p.ordinal,CASE WHEN octet_length(p.node_id)<=512 THEN p.node_id END, n.kind IN ('user','user_message') AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL FROM scope p JOIN node n ON n.id=p.node_id ORDER BY p.ordinal ASC"
    };
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query(params![
        thread,
        boundary.unwrap_or(i64::MAX),
        i64::from(MAX_LIST_LIMIT) + 1
    ])?;
    let limited = || {
        ApiError::new(
            ErrorCode::InputLimit,
            "Saved input exceeds recall's bounded source allowance; no text was truncated",
        )
    };
    let mut last = None;
    for _ in 0..MAX_LIST_LIMIT {
        let Some(row) = rows.next()? else {
            return Ok(Ok(HistoryRecallResult::End));
        };
        let ordinal = u64::try_from(row.get::<_, i64>(0)?)
            .map_err(|_| StorageError::Schema("Negative selected path ordinal".into()))?;
        let node_id: Option<String> = row.get(1)?;
        if !row.get::<_, bool>(2)? {
            last = node_id.map(|node_id| HistoryRecallAnchor { ordinal, node_id });
            continue;
        }
        let Some(node_id) = node_id else {
            return Ok(Err(limited()));
        };
        let source_limit = MAX_FRAME_BYTES as i64;
        let body: Option<String> = db.query_row(
            "SELECT CASE WHEN octet_length(payload_json)<=?2 THEN payload_json END FROM node WHERE id=?1",
            params![node_id, source_limit], |row| row.get(0),
        )?;
        let Some(body) = body else {
            return Ok(Err(limited()));
        };
        let payload = serde_json::from_str(&body)
            .map_err(|_| StorageError::Schema("Saved input is malformed".into()))?;
        let text = crate::history::text_of_payload(&payload);
        if text.len() as u64 > limit {
            return Ok(Err(ApiError::new(
                ErrorCode::InputLimit,
                "Saved input exceeds the current draft limit; no text was truncated",
            )));
        }
        return Ok(Ok(HistoryRecallResult::Found {
            input: HistoryRecalledInput {
                ordinal,
                node_id,
                text,
            },
        }));
    }
    if rows.next()?.is_none() {
        return Ok(Ok(HistoryRecallResult::End));
    }
    let Some(boundary) = last else {
        return Ok(Err(limited()));
    };
    Ok(Ok(HistoryRecallResult::NotInWindow { boundary, revision }))
}

#[cfg(test)]
fn query(
    db: &Connection,
    thread: &str,
    before: Option<u64>,
    node: Option<&str>,
    older: bool,
    limit: u64,
) -> Result<Result<Option<HistoryRecalledInput>, ApiError>, StorageError> {
    if before.is_some() != node.is_some() {
        return Ok(Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Recall cursor requires ordinal and ID",
        )));
    }
    read(
        db,
        &HistoryRecallArgs {
            thread_id: thread.into(),
            older,
            anchor: before
                .zip(node)
                .map(|(ordinal, node_id)| HistoryRecallAnchor {
                    ordinal,
                    node_id: node_id.into(),
                }),
            expected_revision: None,
        },
        limit,
    )
    .map(|result| {
        result.map(|page| match page {
            HistoryRecallResult::Found { input } => Some(input),
            HistoryRecallResult::End => None,
            other => panic!("unexpected recall page {other:?}"),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE thread(id TEXT PRIMARY KEY,selection_revision INTEGER,deleted_at TEXT);
             CREATE TABLE node(id TEXT PRIMARY KEY,kind TEXT,payload_json TEXT,deleted_reason TEXT);
             CREATE TABLE thread_path(thread_id TEXT,ordinal INTEGER,node_id TEXT,PRIMARY KEY(thread_id,ordinal),UNIQUE(thread_id,node_id));
             INSERT INTO thread VALUES('one',4,NULL),('two',2,NULL),('deleted',0,'yesterday');",
        )
        .unwrap();
        db
    }

    fn input(db: &Connection, thread: &str, ordinal: i64, kind: &str, text: &str) {
        let id = format!("{thread}-{ordinal}");
        let payload = serde_json::json!({"role":"user","content":[{"type":"input_text","text":text},{"type":"input_image","image_url":"file:///unavailable.png"}]}).to_string();
        db.execute(
            "INSERT INTO node VALUES(?1,?2,?3,NULL)",
            params![id, kind, payload],
        )
        .unwrap();
        db.execute(
            "INSERT INTO thread_path VALUES(?1,?2,?3)",
            params![thread, ordinal, id],
        )
        .unwrap();
    }

    fn get(db: &Connection, before: Option<u64>, older: bool) -> Option<(u64, String)> {
        let node = before.map(|ordinal| format!("one-{ordinal}"));
        pair(
            query(db, "one", before, node.as_deref(), older, 1_000_000)
                .unwrap()
                .unwrap(),
        )
    }

    fn pair(value: Option<HistoryRecalledInput>) -> Option<(u64, String)> {
        value.map(|prompt| (prompt.ordinal, prompt.text))
    }

    #[test]
    fn saved_inputs_follow_path_ordinals_in_both_directions_and_skip_non_users() {
        let db = database();
        input(&db, "one", 0, "user", "first");
        input(&db, "one", 1, "assistant", "answer");
        input(&db, "one", 2, "user", "second");
        input(&db, "one", 3, "user_message", "third");
        assert_eq!(get(&db, None, true), Some((3, "third".into())));
        assert_eq!(get(&db, Some(3), true), Some((2, "second".into())));
        assert_eq!(get(&db, Some(2), true), Some((0, "first".into())));
        assert_eq!(get(&db, Some(0), true), None);
        assert_eq!(get(&db, Some(0), false), Some((2, "second".into())));
        assert_eq!(get(&db, Some(2), false), Some((3, "third".into())));
        assert_eq!(get(&db, Some(3), false), None);
        assert_eq!(get(&db, None, false), None);
    }

    #[test]
    fn inputs_are_limited_to_selected_path_including_shared_fork_nodes() {
        let db = database();
        input(&db, "one", 0, "user", "shared");
        input(&db, "one", 1, "user", "only parent");
        input(&db, "two", 1, "user", "only child");
        db.execute("INSERT INTO thread_path VALUES('two',0,'one-0')", [])
            .unwrap();
        assert_eq!(get(&db, None, true), Some((1, "only parent".into())));
        assert_eq!(
            pair(query(&db, "two", None, None, true, 100).unwrap().unwrap()),
            Some((1, "only child".into()))
        );
        assert_eq!(
            pair(
                query(&db, "two", Some(1), Some("two-1"), true, 100)
                    .unwrap()
                    .unwrap()
            ),
            Some((0, "shared".into()))
        );
    }

    #[test]
    fn recalls_full_unicode_multiline_text_without_attachments_or_64k_truncation() {
        let db = database();
        let text = format!("  日本語🙂e\u{301}\n{}\nlast\n", "長い行\n".repeat(8000));
        assert!(text.len() > 65_536);
        input(&db, "one", 0, "user", &text);
        assert_eq!(get(&db, None, true), Some((0, text)));
    }

    #[test]
    fn skips_deleted_bodies_and_has_no_fallback_outside_selected_path() {
        let db = database();
        input(&db, "one", 0, "user", "available");
        input(&db, "one", 1, "user", "expired");
        input(&db, "one", 2, "user", "tombstone");
        db.execute("UPDATE node SET payload_json=NULL WHERE id='one-1'", [])
            .unwrap();
        db.execute(
            "UPDATE node SET deleted_reason='removed' WHERE id='one-2'",
            [],
        )
        .unwrap();
        assert_eq!(get(&db, None, true), Some((0, "available".into())));
        assert_eq!(get(&db, Some(0), false), None);
        assert_eq!(
            pair(query(&db, "two", None, None, true, 100).unwrap().unwrap()),
            None
        );
    }

    #[test]
    fn applies_byte_limit_without_truncating_or_silently_skipping_large_inputs() {
        let db = database();
        input(&db, "one", 0, "user", "older");
        input(&db, "one", 1, "user", "日本語");
        assert_eq!(
            query(&db, "one", None, None, true, 8)
                .unwrap()
                .unwrap_err()
                .code,
            ErrorCode::InputLimit
        );
        assert_eq!(
            pair(query(&db, "one", None, None, true, 9).unwrap().unwrap()),
            Some((1, "日本語".into()))
        );
    }

    #[test]
    fn rejects_displaced_cursor_and_unavailable_threads_before_reading_payloads() {
        let db = database();
        input(&db, "one", 0, "user", "input");
        assert_eq!(
            query(&db, "one", Some(0), Some("displaced-node"), true, 100)
                .unwrap()
                .unwrap_err()
                .code,
            ErrorCode::CursorExpired
        );
        for thread in ["missing", "deleted"] {
            assert_eq!(
                query(&db, thread, None, None, true, 100)
                    .unwrap()
                    .unwrap_err()
                    .code,
                ErrorCode::TargetUnavailable
            );
        }
    }

    #[test]
    fn cursor_stays_valid_when_new_nodes_increment_revision_while_viewport_is_pinned() {
        let db = database();
        input(&db, "one", 0, "user", "original");
        let recalled = query(&db, "one", None, None, true, 100)
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(recalled.node_id, "one-0");
        input(&db, "one", 1, "assistant", "new answer");
        input(&db, "one", 2, "user", "new request");
        db.execute(
            "UPDATE thread SET selection_revision=100 WHERE id='one'",
            [],
        )
        .unwrap();
        assert_eq!(
            pair(
                query(
                    &db,
                    "one",
                    Some(recalled.ordinal),
                    Some(&recalled.node_id),
                    false,
                    100
                )
                .unwrap()
                .unwrap()
            ),
            Some((2, "new request".into()))
        );
        assert_eq!(get(&db, None, true), Some((2, "new request".into())));
    }

    #[test]
    fn replaced_and_removed_path_cursors_are_rejected_then_latest_can_be_recalled() {
        let db = database();
        input(&db, "one", 0, "user", "first");
        input(&db, "one", 1, "user", "second");
        // A replacement can occupy the same ordinal without deleting the
        // previous node from the DAG. Ordinal alone must never identify it.
        db.execute(
            "DELETE FROM thread_path WHERE thread_id='one' AND ordinal=0",
            [],
        )
        .unwrap();
        db.execute(
            "UPDATE thread_path SET node_id='one-0' WHERE thread_id='one' AND ordinal=1",
            [],
        )
        .unwrap();
        assert_eq!(
            query(&db, "one", Some(1), Some("one-1"), true, 100)
                .unwrap()
                .unwrap_err()
                .code,
            ErrorCode::CursorExpired
        );
        assert_eq!(get(&db, None, true), Some((1, "first".into())));
        db.execute("INSERT INTO thread_path VALUES('one',0,'one-1')", [])
            .unwrap();
        db.execute(
            "DELETE FROM thread_path WHERE thread_id='one' AND ordinal=1",
            [],
        )
        .unwrap();
        assert_eq!(
            query(&db, "one", Some(1), Some("one-0"), true, 100)
                .unwrap()
                .unwrap_err()
                .code,
            ErrorCode::CursorExpired
        );
        assert_eq!(get(&db, None, true), Some((0, "second".into())));
    }

    #[test]
    fn rejects_incomplete_or_out_of_range_cursors() {
        let db = database();
        for (ordinal, node_id) in [
            (None, Some("one-0")),
            (Some(0), None),
            (Some(u64::MAX), Some("one-0")),
        ] {
            assert_eq!(
                query(&db, "one", ordinal, node_id, true, 100)
                    .unwrap()
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument
            );
        }
    }
}
