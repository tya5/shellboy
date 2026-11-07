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

pub(crate) const TOTAL_SQL: &str =
    "SELECT ordinal+1 FROM thread_path WHERE thread_id=?1 ORDER BY ordinal DESC LIMIT 1";
pub(crate) const NODE_SQL: &str =
    "SELECT ordinal FROM thread_path WHERE thread_id=?1 AND node_id=?2";
pub(crate) const ORDINAL_SQL: &str =
    "SELECT node_id FROM thread_path WHERE thread_id=?1 AND ordinal=?2";
pub(crate) const WINDOW_SQL: &str = "SELECT n.id,n.kind,CASE WHEN ?4=0 THEN NULL ELSE n.payload_json END,n.deleted_reason,p.ordinal,n.created_at,length(n.payload_json) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id=?1 AND p.ordinal>=?2 AND p.ordinal<=?3 ORDER BY p.ordinal";

/// Integration-test hook: the statements whose plans the cost tests inspect.
#[doc(hidden)]
pub const STATEMENTS_FOR_TESTS: [&str; 4] = [TOTAL_SQL, NODE_SQL, ORDINAL_SQL, WINDOW_SQL];
