//! One saved user input at a time, independently of the viewport/body cache.
use super::{checked_storage, storage_error};
use crate::{control::Control, protocol::*, storage::StorageError, ui::RecalledPrompt};
use rusqlite::{Connection, OptionalExtension, params};

type RecalledInput = Option<RecalledPrompt>;

pub(super) async fn recall(
    control: &Control,
    thread_id: String,
    before_ordinal: Option<u64>,
    before_node_id: Option<String>,
    older: bool,
) -> Result<RecalledInput, ApiError> {
    let limit = control.store.config.limits.draft_bytes;
    let result = control
        .store
        .database
        .call(move |db| {
            query(
                db,
                &thread_id,
                before_ordinal,
                before_node_id.as_deref(),
                older,
                limit,
            )
        })
        .await
        .map_err(storage_error)
        .and_then(|result| result);
    checked_storage(control, result).await
}

fn query(
    db: &Connection,
    thread: &str,
    before: Option<u64>,
    before_node_id: Option<&str>,
    older: bool,
    limit: u64,
) -> Result<Result<RecalledInput, ApiError>, StorageError> {
    let live: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [thread],
        |row| row.get(0),
    )?;
    if !live {
        return Ok(Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            "Input history Thread is no longer available",
        )));
    }
    if before.is_some() != before_node_id.is_some() {
        return Ok(Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Input history cursor requires both ordinal and node ID",
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
        return Ok(Ok(None));
    }
    // Tombstones are not editable inputs. Filtering them here also avoids a
    // potentially unbounded loop over reclaimed nodes on the async adapter.
    let sql = if older {
        "SELECT p.ordinal,n.id,n.payload_json FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND (?2 IS NULL OR p.ordinal<?2) AND n.kind IN ('user','user_message') AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL ORDER BY p.ordinal DESC LIMIT 1"
    } else {
        "SELECT p.ordinal,n.id,n.payload_json FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>?2 AND n.kind IN ('user','user_message') AND n.payload_json IS NOT NULL AND n.deleted_reason IS NULL ORDER BY p.ordinal ASC LIMIT 1"
    };
    let row: Option<(i64, String, String)> = db
        .query_row(sql, params![thread, boundary], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let Some((ordinal, node_id, body)) = row else {
        return Ok(Ok(None));
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
    Ok(Ok(Some(RecalledPrompt {
        ordinal: ordinal as u64,
        node_id,
        text,
    })))
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

    fn pair(value: RecalledInput) -> Option<(u64, String)> {
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
