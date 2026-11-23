//! Live export of retained nodes. The cursor follows the saved node insertion order,
//! not the selected-path ordinal (which changes when a replacement is adopted).
use super::{relations, sql_error};
use crate::protocol::*;
use rusqlite::{Connection, params};

const SCOPE: &str = "history.export";
const MEMBERS: &str = "WITH RECURSIVE members(id) AS (
    SELECT node_id FROM thread_path WHERE thread_id=?1
    UNION
    SELECT s.source_node_id FROM members m JOIN replacement r ON r.output_node_id=m.id
    JOIN replacement_source s ON s.replacement_id=r.id WHERE r.state IN ('completed','pruned')
)";

pub(crate) fn read(
    db: &Connection,
    args: &HistoryExportArgs,
    owner: &str,
    operation: &str,
) -> Result<HistoryExportPage, ApiError> {
    let filter = serde_json::json!([owner, args.thread_id]).to_string();
    let cursor_error = || ApiError::new(ErrorCode::CursorExpired, "invalid Thread export cursor");
    let after = args
        .cursor
        .as_deref()
        .map(|token| {
            PageCursor::decode(token, SCOPE, &filter, 0, None)?
                .last_sort_key
                .parse::<i64>()
                .ok()
                .filter(|position| *position > 0)
                .ok_or_else(cursor_error)
        })
        .transpose()?
        .unwrap_or(0);
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=? AND deleted_at IS NULL)",
            [&args.thread_id],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if !exists {
        return Err(ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"));
    }
    let limit = args.limit as usize;
    let mut page = HistoryExportPage {
        items: Vec::new(),
        next_cursor: None,
        change_position: u64::MAX,
    };
    let mut keys = db
        .prepare(&format!(
            "{MEMBERS}
        SELECT n.rowid,n.id,p.ordinal FROM members m JOIN node n ON n.id=m.id
        LEFT JOIN thread_path p ON p.thread_id=?1 AND p.node_id=n.id
        WHERE n.rowid>?2 ORDER BY n.rowid LIMIT ?3"
        ))
        .map_err(sql_error)?;
    let mut rows = keys
        .query(params![args.thread_id, after, limit as i64 + 1])
        .map_err(sql_error)?;
    let mut position = after;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let next = |position: i64| {
            PageCursor {
                version: 1,
                scope: SCOPE.into(),
                filter: filter.clone(),
                last_sort_key: position.to_string(),
                watermark: 0,
                revision: 0,
                encoding: None,
            }
            .encode()
        };
        if page.items.len() == limit {
            page.next_cursor = Some(next(position)?);
            break;
        }
        let id: String = row.get(1).map_err(sql_error)?;
        let record = read_node(
            db,
            &args.thread_id,
            &id,
            row.get(0).map_err(sql_error)?,
            row.get(2).map_err(sql_error)?,
        );
        let record = match record {
            Err(error) if error.code == ErrorCode::InputLimit && !page.items.is_empty() => {
                page.next_cursor = Some(next(position)?);
                break;
            }
            other => other?,
        };
        let key: i64 = row.get(0).map_err(sql_error)?;
        page.items.push(record);
        page.next_cursor = Some(next(key)?);
        if !super::message_budget::fits_complete(operation, &page).map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "cannot serialize Thread export page",
            )
        })? {
            page.items.pop();
            if page.items.is_empty() {
                return Err(oversized(&id));
            }
            page.next_cursor = Some(next(position)?);
            break;
        }
        position = key;
        page.next_cursor = None;
    }
    Ok(page)
}

fn oversized(id: &str) -> ApiError {
    ApiError::new(
        ErrorCode::InputLimit,
        "history export node cannot fit in one response",
    )
    .details(serde_json::json!({"node_id":id}))
}

fn read_node(
    db: &Connection,
    thread: &str,
    id: &str,
    order: i64,
    current_ordinal: Option<i64>,
) -> Result<HistoryExportRecord, ApiError> {
    let (len, payload_json, reason): (Option<i64>, Option<String>, Option<String>) = db.query_row(
        "SELECT octet_length(payload_json),CASE WHEN octet_length(payload_json)<=?2 THEN payload_json END,deleted_reason FROM node WHERE id=?1",
        params![id, MAX_FRAME_BYTES as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).map_err(sql_error)?;
    if payload_json.is_none() && len.is_some() {
        return Err(oversized(id));
    }
    let provenance = relations::projected_provenance(db, id, None)?;
    let owner_thread_id = provenance.origin_thread_id.clone();
    let mut graph = GraphReader {
        db,
        thread,
        id,
        remaining: MAX_FRAME_BYTES - payload_json.as_ref().map_or(0, String::len),
    };
    let relations = graph.relations()?;
    let role = if current_ordinal.is_some() {
        HistoryExportRole::Current
    } else {
        HistoryExportRole::ReplacedSource
    };
    let node = Box::new(HistoryExportNode {
        id: id.into(),
        order,
        current_ordinal,
        payload_json,
        provenance,
    });
    Ok(if node.payload_json.is_some() {
        HistoryExportRecord::Node {
            role,
            node,
            relations,
            owner_thread_id,
        }
    } else {
        HistoryExportRecord::Tombstone {
            role,
            node,
            relations,
            owner_thread_id,
            reason: reason.unwrap_or_else(|| "body unavailable".into()),
        }
    })
}

struct GraphReader<'a> {
    db: &'a Connection,
    thread: &'a str,
    id: &'a str,
    remaining: usize,
}

impl GraphReader<'_> {
    fn relations(&mut self) -> Result<HistoryExportRelations, ApiError> {
        let previous = self.read(
            "SELECT src AS target FROM edge WHERE dst=?2",
            "g.target",
            |_, target| Ok(target),
        )?;
        let next = self.read(
            "SELECT dst AS target FROM edge WHERE src=?2",
            "g.target",
            |_, target| Ok(target),
        )?;
        let rows = self.read(
            "SELECT r.id,r.state,s.ordinal,s.source_node_id AS target FROM replacement r
             LEFT JOIN replacement_source s ON s.replacement_id=r.id
             WHERE r.output_node_id=?2 AND r.state IN ('completed','pruned')",
            "g.id,g.ordinal",
            |row, target| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    target,
                ))
            },
        )?;
        let mut replacement_groups: Vec<HistoryExportGroup> = Vec::new();
        for (id, state, ordinal, node) in rows {
            if replacement_groups.last().is_none_or(|group| group.id != id) {
                replacement_groups.push(HistoryExportGroup {
                    id,
                    state,
                    sources: Vec::new(),
                });
            }
            if let Some(ordinal) = ordinal {
                replacement_groups
                    .last_mut()
                    .expect("group was added")
                    .sources
                    .push(HistoryExportSource {
                        ordinal,
                        node: node.expect("source id is non-null"),
                    });
            }
        }
        let replaced_by = self.read(
            "SELECT r.id,s.ordinal,r.output_node_id AS target FROM replacement_source s
             JOIN replacement r ON r.id=s.replacement_id WHERE s.source_node_id=?2 AND r.state IN ('completed','pruned')",
            "g.id,g.ordinal", |row, output| Ok(HistoryExportMembership { group_id: row.get(0)?, ordinal: row.get(1)?, output: output.expect("output id is non-null") }),
        )?;
        Ok(HistoryExportRelations {
            previous: previous.into_iter().flatten().collect(),
            next: next.into_iter().flatten().collect(),
            replacement_groups,
            replaced_by,
        })
    }

    fn read<T>(
        &mut self,
        query: &str,
        order_by: &str,
        mut project: impl FnMut(
            &rusqlite::Row<'_>,
            Option<HistoryExportReference>,
        ) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, ApiError> {
        let mut stmt = self
            .db
            .prepare(&format!(
                "{MEMBERS}, graph AS ({query})
            SELECT g.*, CASE WHEN n.id IS NULL THEN 0 WHEN m.id IS NULL THEN 1 ELSE 2 END
            FROM graph g LEFT JOIN node n ON n.id=g.target LEFT JOIN members m ON m.id=g.target
            ORDER BY {order_by}"
            ))
            .map_err(sql_error)?;
        let column_count = stmt.column_count();
        let mut rows = stmt
            .query(params![self.thread, self.id])
            .map_err(sql_error)?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(sql_error)? {
            // Bound retained relation rows before allocating them; exact frame fitting follows.
            let mut bytes = 16;
            for column in 0..column_count - 1 {
                if let rusqlite::types::ValueRef::Text(value) =
                    row.get_ref(column).map_err(sql_error)?
                {
                    bytes += value.len();
                }
            }
            self.remaining = self
                .remaining
                .checked_sub(bytes)
                .ok_or_else(|| oversized(self.id))?;
            let scope = match row.get::<_, i64>(column_count - 1).map_err(sql_error)? {
                0 => HistoryExportReferenceScope::Missing,
                1 => HistoryExportReferenceScope::External,
                _ => HistoryExportReferenceScope::Internal,
            };
            let target = row
                .get::<_, Option<String>>(column_count - 2)
                .map_err(sql_error)?
                .map(|id| HistoryExportReference { id, scope });
            result.push(project(row, target).map_err(sql_error)?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
