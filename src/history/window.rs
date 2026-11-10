//! Positional read of a Thread's selected path (#268). Every statement is a
//! primary-key or unique-index seek plus a bounded range: the cost follows the
//! window, never the path length.
use super::*;

/// `thread_path` ordinals are contiguous from 0 in every production writer
/// (append is MAX+1, path adoption rewrites 0..n, fork copies a prefix), so
/// `total` is one past the last ordinal. It is read as a seek, not counted.
pub fn window(
    connection: &Connection,
    args: &crate::protocol::HistoryWindowArgs,
) -> Result<crate::protocol::HistoryWindow, ApiError> {
    if args.items {
        return window_with_items(connection, args);
    }
    let invalid = |message| ApiError::new(ErrorCode::InvalidArgument, message);
    if args.ordinal.is_some() && args.node_id.is_some() {
        return Err(invalid("give at most one of ordinal and node_id"));
    }
    if args.expect_node_id.is_some() && args.ordinal.is_none() {
        return Err(invalid("expect_node_id needs ordinal"));
    }
    if u64::from(args.before) + u64::from(args.after) + 1 > u64::from(MAX_LIST_LIMIT) {
        return Err(invalid("before + after + 1 must be at most 200"));
    }
    if args.excerpt_chars > crate::protocol::MAX_HISTORY_EXCERPT_CHARS {
        return Err(invalid("excerpt-chars must be in 0..=4096"));
    }
    let (revision, current_cwd): (u64, String) = connection
        .query_row(
            "SELECT selection_revision,cwd FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [&args.thread_id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"))?;
    let total: u64 = connection
        .query_row(TOTAL_SQL, [&args.thread_id], |row| {
            row.get::<_, i64>(0).map(|n| n as u64)
        })
        .optional()
        .map_err(sql_error)?
        .unwrap_or(0);
    let stale = |reason: &str| {
        ApiError::new(ErrorCode::CursorExpired, reason)
            .details(serde_json::json!({"revision": revision, "total": total}))
    };
    let anchor = if let Some(node_id) = &args.node_id {
        connection
            .query_row(NODE_SQL, params![args.thread_id, node_id], |row| {
                row.get::<_, i64>(0).map(|n| n as u64)
            })
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| stale("node is not on the selected path"))?
    } else if let Some(ordinal) = args.ordinal {
        let node: Option<String> = connection
            .query_row(
                ORDINAL_SQL,
                params![args.thread_id, ordinal as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        let node = node.ok_or_else(|| stale("position is past the end of the selected path"))?;
        if args.expect_node_id.as_ref().is_some_and(|e| *e != node) {
            return Err(stale("selected path changed at this position"));
        }
        ordinal
    } else if total == 0 {
        return Ok(crate::protocol::HistoryWindow {
            thread_id: args.thread_id.clone(),
            revision,
            total,
            items: Vec::new(),
        });
    } else {
        total - 1
    };
    let low = anchor.saturating_sub(u64::from(args.before));
    let high = anchor.saturating_add(u64::from(args.after));
    let mut statement = connection.prepare_cached(WINDOW_SQL).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            args.thread_id,
            low as i64,
            high.min(i64::MAX as u64) as i64,
            args.excerpt_chars
        ])
        .map_err(sql_error)?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        items.push(
            list::history_snippet(
                connection,
                row,
                &args.thread_id,
                &current_cwd,
                args.excerpt_chars,
            )?
            .0,
        );
    }
    Ok(crate::protocol::HistoryWindow {
        thread_id: args.thread_id.clone(),
        revision,
        total,
        items,
    })
}

struct ItemWindowRow {
    node_id: String,
    kind: String,
    deleted_reason: Option<String>,
    ordinal: u64,
    payload_bytes: i64,
}

fn unavailable_node_item(
    node_id: &str,
    reason: crate::protocol::HistoryItemUnavailableReason,
) -> crate::protocol::HistoryItem {
    crate::protocol::HistoryItem::Unavailable {
        item_identity: serde_json::to_string(&(node_id, ""))
            .expect("node/item identity serializes"),
        reason,
    }
}

fn unavailable_projection(
    node_id: &str,
    reason: crate::protocol::HistoryItemUnavailableReason,
) -> crate::protocol::HistoryNodeItems {
    crate::protocol::HistoryNodeItems {
        items: Some(vec![unavailable_node_item(node_id, reason)]),
        items_unavailable_reason: None,
    }
}

fn response_budget_projection() -> crate::protocol::HistoryNodeItems {
    crate::protocol::HistoryNodeItems {
        items: None,
        items_unavailable_reason: Some(
            crate::protocol::HistoryItemUnavailableReason::ResponseBudget,
        ),
    }
}

/// Opt-in item projection. Metadata is scanned first; payload JSON is fetched
/// only for a node whose stored UTF-8 byte count admits it under the source cap.
fn window_with_items(
    connection: &Connection,
    args: &crate::protocol::HistoryWindowArgs,
) -> Result<crate::protocol::HistoryWindow, ApiError> {
    use crate::history::items;
    use crate::protocol::HistoryItemUnavailableReason as Unavailable;
    let invalid = |message| ApiError::new(ErrorCode::InvalidArgument, message);
    if args.ordinal.is_some() && args.node_id.is_some() {
        return Err(invalid("give at most one of ordinal and node_id"));
    }
    if args.expect_node_id.is_some() && args.ordinal.is_none() {
        return Err(invalid("expect_node_id needs ordinal"));
    }
    if u64::from(args.before) + u64::from(args.after) + 1 > u64::from(MAX_LIST_LIMIT) {
        return Err(invalid("before + after + 1 must be at most 200"));
    }
    if args.excerpt_chars > crate::protocol::MAX_HISTORY_EXCERPT_CHARS {
        return Err(invalid("excerpt-chars must be in 0..=4096"));
    }

    // Reuse the caller's existing read transaction. Public HistoryWindow is
    // dispatched inside the inspection transaction; a nested BEGIN on the same
    // SQLite connection fails and would also split the caller's snapshot.
    let snapshot = connection;
    let (revision, current_cwd): (u64, String) = snapshot
        .query_row(
            "SELECT selection_revision,cwd FROM thread WHERE id=?1 AND deleted_at IS NULL",
            [&args.thread_id],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"))?;
    let total: u64 = snapshot
        .query_row(TOTAL_SQL, [&args.thread_id], |row| {
            row.get::<_, i64>(0).map(|n| n as u64)
        })
        .optional()
        .map_err(sql_error)?
        .unwrap_or(0);
    let stale = |reason: &str| {
        ApiError::new(ErrorCode::CursorExpired, reason)
            .details(serde_json::json!({"revision": revision, "total": total}))
    };
    let anchor = if let Some(node_id) = &args.node_id {
        snapshot
            .query_row(NODE_SQL, params![args.thread_id, node_id], |row| {
                row.get::<_, i64>(0).map(|n| n as u64)
            })
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| stale("node is not on the selected path"))?
    } else if let Some(ordinal) = args.ordinal {
        let node: Option<String> = snapshot
            .query_row(
                ORDINAL_SQL,
                params![args.thread_id, ordinal as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        let node = node.ok_or_else(|| stale("position is past the end of the selected path"))?;
        if args
            .expect_node_id
            .as_ref()
            .is_some_and(|expected| *expected != node)
        {
            return Err(stale("selected path changed at this position"));
        }
        ordinal
    } else if total == 0 {
        return Ok(crate::protocol::HistoryWindow {
            thread_id: args.thread_id.clone(),
            revision,
            total,
            items: Vec::new(),
        });
    } else {
        total - 1
    };
    let low = anchor.saturating_sub(u64::from(args.before));
    let high = anchor.saturating_add(u64::from(args.after));
    let rows: Vec<ItemWindowRow> = {
        let mut statement = snapshot
            .prepare_cached(WINDOW_ITEMS_METADATA_SQL)
            .map_err(sql_error)?;
        let mapped = statement
            .query_map(
                params![args.thread_id, low as i64, high.min(i64::MAX as u64) as i64,],
                |row| {
                    Ok(ItemWindowRow {
                        node_id: row.get(0)?,
                        kind: row.get(1)?,
                        deleted_reason: row.get(2)?,
                        ordinal: row.get::<_, i64>(3)? as u64,
                        payload_bytes: row.get(4)?,
                    })
                },
            )
            .map_err(sql_error)?;
        mapped
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
    };

    let mut response_text_bytes = 0usize;
    let mut response_budget_exhausted = false;
    let mut window_items = Vec::with_capacity(rows.len());
    for row in rows {
        let mut body: Option<serde_json::Value> = None;
        let mut payload_chars = None;
        let item_projection = if response_budget_exhausted {
            response_budget_projection()
        } else if row.deleted_reason.is_some() {
            unavailable_projection(&row.node_id, Unavailable::Pruned)
        } else if row.payload_bytes <= 0 {
            unavailable_projection(&row.node_id, Unavailable::Legacy)
        } else if row.payload_bytes as u64 > items::NODE_SOURCE_BYTES as u64 {
            unavailable_projection(&row.node_id, Unavailable::SourceBudget)
        } else {
            let loaded: Option<(String, i64)> = snapshot
                .query_row(
                    ITEM_PAYLOAD_SQL,
                    params![
                        row.node_id,
                        row.payload_bytes,
                        items::NODE_SOURCE_BYTES as i64
                    ],
                    |payload_row| Ok((payload_row.get(0)?, payload_row.get(1)?)),
                )
                .optional()
                .map_err(sql_error)?;
            let Some((payload_json, chars)) = loaded else {
                window_items.push(list::history_snippet_with_items(
                    &snapshot,
                    &args.thread_id,
                    &current_cwd,
                    row.node_id.clone(),
                    row.kind.clone(),
                    row.deleted_reason.clone(),
                    row.ordinal,
                    None,
                    None,
                    args.excerpt_chars,
                    unavailable_projection(&row.node_id, Unavailable::Legacy),
                    Vec::new(),
                    0,
                )?);
                continue;
            };
            payload_chars = Some(chars.max(0) as u64);
            let payload: serde_json::Value = serde_json::from_str(&payload_json).map_err(|_| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "stored history payload is malformed",
                )
            })?;
            let (links, link_count) = execution_links(&snapshot, &row.node_id)?;
            let projected = items::project(&row.node_id, &row.kind, &payload, &links);
            let text_bytes = items::text_bytes(&projected);
            if response_text_bytes.saturating_add(text_bytes) > items::RESPONSE_TEXT_BYTES {
                response_budget_exhausted = true;
                response_budget_projection()
            } else {
                response_text_bytes += text_bytes;
                body = Some(payload);
                window_items.push(list::history_snippet_with_items(
                    &snapshot,
                    &args.thread_id,
                    &current_cwd,
                    row.node_id,
                    row.kind,
                    row.deleted_reason,
                    row.ordinal,
                    payload_chars,
                    body.as_ref(),
                    args.excerpt_chars,
                    crate::protocol::HistoryNodeItems {
                        items: Some(projected),
                        items_unavailable_reason: None,
                    },
                    links,
                    link_count,
                )?);
                continue;
            }
        };
        let (links, link_count) = execution_links(&snapshot, &row.node_id)?;
        window_items.push(list::history_snippet_with_items(
            &snapshot,
            &args.thread_id,
            &current_cwd,
            row.node_id,
            row.kind,
            row.deleted_reason,
            row.ordinal,
            payload_chars,
            body.as_ref(),
            args.excerpt_chars,
            item_projection,
            links,
            link_count,
        )?);
    }
    Ok(crate::protocol::HistoryWindow {
        thread_id: args.thread_id.clone(),
        revision,
        total,
        items: window_items,
    })
}

pub(crate) const TOTAL_SQL: &str =
    "SELECT ordinal+1 FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1";
pub(crate) const NODE_SQL: &str =
    "SELECT ordinal FROM thread_path WHERE thread_id=?1 AND node_id=?2";
pub(crate) const ORDINAL_SQL: &str =
    "SELECT node_id FROM thread_path WHERE thread_id=?1 AND ordinal=?2";
pub(crate) const WINDOW_SQL: &str = "SELECT n.id,n.kind,CASE WHEN ?4=0 THEN NULL ELSE n.payload_json END,n.deleted_reason,p.ordinal,n.created_at,length(n.payload_json) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<=?3 ORDER BY p.ordinal";
/// Item windows read metadata first; payload JSON is fetched only by NODE_SOURCE_BYTES admission.
const WINDOW_ITEMS_METADATA_SQL: &str = "SELECT n.id,n.kind,n.deleted_reason,p.ordinal,n.payload_bytes FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<=?3 ORDER BY p.ordinal";
const ITEM_PAYLOAD_SQL: &str = "SELECT payload_json,length(payload_json) FROM node WHERE id=?1 AND payload_bytes=?2 AND payload_bytes>0 AND payload_bytes<=?3 AND payload_json IS NOT NULL";

/// Integration-test hook: the statements whose plans the cost tests inspect.
#[doc(hidden)]
pub const STATEMENTS_FOR_TESTS: [&str; 4] = [TOTAL_SQL, NODE_SQL, ORDINAL_SQL, WINDOW_SQL];

#[cfg(test)]
mod items_source_guard_tests {
    use super::*;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn fixture(text: String) -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute(
            "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/p','/p/AGENTS.md','fixture','model','now')",
            [],
        ).unwrap();
        let payload =
            json!({"output":[{"type":"message","content":[{"type":"output_text","text":text}]}]});
        let tx = db.transaction().unwrap();
        crate::history::append_node(&tx, "t", "n", None, "assistant_response", &payload, "now")
            .unwrap();
        tx.commit().unwrap();
        db
    }

    fn payload_read_counter(db: &Connection) -> Arc<AtomicUsize> {
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        db.authorizer(Some(move |context: AuthContext<'_>| {
            if let AuthAction::Read {
                table_name,
                column_name,
            } = context.action
                && table_name == "node"
                && column_name == "payload_json"
            {
                observed.fetch_add(1, Ordering::Relaxed);
            }
            Authorization::Allow
        }))
        .unwrap();
        reads
    }

    fn args(items: bool) -> crate::protocol::HistoryWindowArgs {
        crate::protocol::HistoryWindowArgs {
            thread_id: "t".into(),
            items,
            ordinal: Some(0),
            node_id: None,
            expect_node_id: Some("n".into()),
            before: 0,
            after: 0,
            excerpt_chars: 32,
        }
    }

    #[test]
    fn items_false_retains_legacy_payload_read_and_projection() {
        let db = fixture("hello".into());
        let reads = payload_read_counter(&db);
        let output = window(&db, &args(false)).unwrap();
        assert_eq!(output.items[0].excerpt, "hello");
        assert!(output.items[0].item_projection.is_none());
        assert!(
            reads.load(Ordering::Relaxed) >= 1,
            "legacy path reads its selected payload"
        );
    }

    #[test]
    fn items_true_reuses_the_caller_transaction_without_nested_begin() {
        let db = fixture("caller transaction".into());
        let tx = db.unchecked_transaction().unwrap();
        let result = window(&tx, &args(true)).expect("window must reuse caller-owned transaction");
        assert_eq!(result.items.len(), 1);
        assert!(
            result.items[0]
                .item_projection
                .as_ref()
                .unwrap()
                .items
                .is_some()
        );
        tx.commit().unwrap();
    }

    #[test]
    fn items_true_gates_payload_column_on_trusted_utf8_source_bytes() {
        let db = fixture("界".repeat(ITEM_TEXT_BYTES_FOR_TEST));
        let (stored_bytes, sqlite_text_chars): (i64, i64) = db
            .query_row(
                "SELECT payload_bytes,length(payload_json) FROM node WHERE id='n'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(stored_bytes as usize <= crate::history::items::NODE_SOURCE_BYTES);
        assert!(
            stored_bytes > sqlite_text_chars,
            "payload_bytes counts UTF-8 bytes; SQLite length(TEXT) counts characters"
        );
        let reads = payload_read_counter(&db);
        let output = window(&db, &args(true)).unwrap();
        assert!(
            reads.load(Ordering::Relaxed) >= 1,
            "admitted source payload is selected after metadata admission"
        );
        assert_eq!(
            output.items[0]
                .item_projection
                .as_ref()
                .unwrap()
                .items
                .as_ref()
                .unwrap()
                .len(),
            1
        );

        let db = fixture("界".repeat((crate::history::items::NODE_SOURCE_BYTES / 3) + 64));
        let stored_bytes: i64 = db
            .query_row("SELECT payload_bytes FROM node WHERE id='n'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(stored_bytes as usize > crate::history::items::NODE_SOURCE_BYTES);
        let reads = payload_read_counter(&db);
        let output = window(&db, &args(true)).unwrap();
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "oversize payload_json must not be read or materialized"
        );
        let projection = output.items[0].item_projection.as_ref().unwrap();
        assert_eq!(
            projection.items.as_ref().unwrap()[0],
            unavailable_node_item(
                "n",
                crate::protocol::HistoryItemUnavailableReason::SourceBudget
            )
        );
        assert!(!output.items[0].excerpt.contains("界"));

        let db = fixture("legacy-secret".into());
        db.execute("UPDATE node SET payload_bytes=0 WHERE id='n'", [])
            .unwrap();
        let reads = payload_read_counter(&db);
        let output = window(&db, &args(true)).unwrap();
        assert_eq!(
            reads.load(Ordering::Relaxed),
            0,
            "untrusted zero byte metadata never falls back to raw payload"
        );
        let projection = output.items[0].item_projection.as_ref().unwrap();
        assert_eq!(
            projection.items.as_ref().unwrap()[0],
            unavailable_node_item("n", crate::protocol::HistoryItemUnavailableReason::Legacy)
        );
    }

    const ITEM_TEXT_BYTES_FOR_TEST: usize = 96;
}
