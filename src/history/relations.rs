//! Direct conversation edges and replacement witnesses are separate relations.
use rusqlite::{Connection, OptionalExtension, params};

use crate::protocol::{
    ApiError, ErrorCode, HistoryRelation, HistoryRelationsArgs, MAX_LIST_LIMIT, NodeProvenance,
    Page, PageCursor, RelationItem,
};

use super::sql_error;

/// Metadata describes creation, not the potentially many Threads selecting a node.
pub fn provenance(connection: &Connection, node_id: &str) -> Result<NodeProvenance, ApiError> {
    connection
        .query_row(
            "SELECT n.kind,n.created_at,n.payload_revision,n.turn_id,t.run_id,COALESCE(t.thread_id,r.thread_id) FROM node n LEFT JOIN turn t ON t.id=n.turn_id LEFT JOIN run r ON r.id=t.run_id WHERE n.id=?1",
            [node_id],
            |row| Ok(NodeProvenance {
                kind: row.get(0)?,
                created_at: row.get(1)?,
                source_revision: row.get(2)?,
                turn_id: row.get(3)?,
                run_id: row.get(4)?,
                origin_thread_id: row.get(5)?,
            }),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable, "history node not found"))
}

pub fn relations(
    connection: &Connection,
    args: &HistoryRelationsArgs,
) -> Result<Page<RelationItem>, ApiError> {
    if args.limit == 0 || args.limit > MAX_LIST_LIMIT {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "history relation limit must be in 1..=200",
        ));
    }
    let metadata = provenance(connection, &args.node_id)?;
    let revision = metadata.source_revision as u64;
    let relation = match args.relation {
        HistoryRelation::Previous => "previous",
        HistoryRelation::Next => "next",
        HistoryRelation::Sources => "sources",
        HistoryRelation::Replacements => "replacements",
    };
    let filter = serde_json::to_string(&(&args.node_id, relation)).map_err(|_| invalid_cursor())?;
    // Both SQL branches expose one order: group rowid, source ordinal, witness rowid.
    // Source witnesses are ordered by their original range ordinal, not node IDs.
    let (from, constraint, columns, order) = match args.relation {
        HistoryRelation::Previous => (
            "edge e",
            "e.dst=?1",
            "e.src,NULL,NULL,NULL,e.rowid,0,e.rowid",
            "e.rowid,CAST(0 AS INTEGER),e.rowid",
        ),
        HistoryRelation::Next => (
            "edge e",
            "e.src=?1",
            "e.dst,NULL,NULL,NULL,e.rowid,0,e.rowid",
            "e.rowid,CAST(0 AS INTEGER),e.rowid",
        ),
        HistoryRelation::Sources => (
            "replacement_source s JOIN replacement r ON r.id=s.replacement_id",
            "r.output_node_id=?1 AND r.state='completed'",
            "s.source_node_id,r.id,s.source_revision,s.source_hash,r.rowid,s.ordinal,s.rowid",
            "r.rowid,s.ordinal,s.rowid",
        ),
        HistoryRelation::Replacements => (
            "replacement_source s JOIN replacement r ON r.id=s.replacement_id",
            "s.source_node_id=?1 AND r.state='completed'",
            "r.output_node_id,r.id,s.source_revision,s.source_hash,r.rowid,s.ordinal,s.rowid",
            "r.rowid,s.ordinal,s.rowid",
        ),
    };
    let is_edge = matches!(
        args.relation,
        HistoryRelation::Previous | HistoryRelation::Next
    );
    let row_id = if is_edge { "e.rowid" } else { "s.rowid" };
    let current_max: i64 = connection
        .query_row(
            &format!("SELECT COALESCE(MAX({row_id}),0) FROM {from} WHERE {constraint}"),
            [&args.node_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let (last, watermark) = if let Some(token) = &args.cursor {
        let cursor = PageCursor::decode(token, "history.relations", &filter, revision, None)?;
        let last: (i64, i64, i64) =
            serde_json::from_str(&cursor.last_sort_key).map_err(|_| invalid_cursor())?;
        if last.0 <= 0
            || last.1 < 0
            || last.2 <= 0
            || last.2 as u64 > cursor.watermark
            || cursor.watermark > i64::MAX as u64
            || cursor.watermark > current_max as u64
        {
            return Err(invalid_cursor());
        }
        (last, cursor.watermark)
    } else {
        ((0, 0, 0), current_max as u64)
    };
    let tuple = format!("({order})");
    let sql = format!(
        "SELECT {columns} FROM {from} WHERE {constraint} AND {row_id}<=?2 AND {tuple}>(?3,?4,?5) ORDER BY {order} LIMIT ?6"
    );
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            args.node_id,
            watermark as i64,
            last.0,
            last.1,
            last.2,
            args.limit + 1
        ])
        .map_err(sql_error)?;
    let mut items = Vec::new();
    let mut position = last;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(sql_error)? {
        if items.len() == args.limit as usize {
            has_more = true;
            break;
        }
        let node_id: String = row.get(0).map_err(sql_error)?;
        items.push(RelationItem {
            provenance: provenance(connection, &node_id)?,
            node_id,
            replacement_id: row.get(1).map_err(sql_error)?,
            source_revision: row.get(2).map_err(sql_error)?,
            source_hash: row.get(3).map_err(sql_error)?,
        });
        position = (
            row.get(4).map_err(sql_error)?,
            row.get(5).map_err(sql_error)?,
            row.get(6).map_err(sql_error)?,
        );
    }
    let next_cursor = if has_more {
        Some(
            PageCursor {
                version: 1,
                scope: "history.relations".into(),
                filter,
                last_sort_key: serde_json::to_string(&position).map_err(|_| invalid_cursor())?,
                watermark,
                revision,
                encoding: None,
            }
            .encode()?,
        )
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

fn invalid_cursor() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "history relation position is unavailable",
    )
}
