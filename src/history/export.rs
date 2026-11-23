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
    id: &str,
    order: i64,
    current_ordinal: Option<i64>,
) -> Result<HistoryExportRecord, ApiError> {
    let (len, payload, reason): (Option<i64>, Option<String>, Option<String>) = db.query_row(
        "SELECT octet_length(payload_json),CASE WHEN octet_length(payload_json)<=?2 THEN payload_json END,deleted_reason FROM node WHERE id=?1",
        params![id, MAX_FRAME_BYTES as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).map_err(sql_error)?;
    let payload_json = match payload {
        Some(value) => value,
        None if len.is_some() => return Err(oversized(id)),
        None => {
            return Ok(HistoryExportRecord::Tombstone {
                id: id.into(),
                reason: reason.unwrap_or_else(|| "body unavailable".into()),
            });
        }
    };
    let provenance = relations::projected_provenance(db, id, None)?;
    let mut remaining = MAX_FRAME_BYTES - payload_json.len();
    let mut read_ids = |sql| relation_ids(db, sql, id, &mut remaining);
    let previous = read_ids("SELECT src FROM edge WHERE dst=?1 ORDER BY src")?;
    let replaces = read_ids(
        "SELECT s.source_node_id FROM replacement r JOIN replacement_source s ON s.replacement_id=r.id WHERE r.output_node_id=?1 AND r.state IN ('completed','pruned') ORDER BY r.rowid,s.ordinal",
    )?;
    let replaced_by = read_ids(
        "SELECT DISTINCT r.output_node_id FROM replacement_source s JOIN replacement r ON r.id=s.replacement_id WHERE s.source_node_id=?1 AND r.state IN ('completed','pruned') ORDER BY r.output_node_id",
    )?;
    Ok(HistoryExportRecord::Node {
        role: if current_ordinal.is_some() {
            HistoryExportRole::Current
        } else {
            HistoryExportRole::ReplacedSource
        },
        owner_thread_id: provenance.origin_thread_id.clone(),
        node: Box::new(HistoryExportNode {
            id: id.into(),
            order,
            current_ordinal,
            payload_json,
            provenance,
        }),
        relations: HistoryExportRelations {
            previous,
            replaces,
            replaced_by,
        },
    })
}

fn relation_ids(
    db: &Connection,
    sql: &str,
    id: &str,
    remaining: &mut usize,
) -> Result<Vec<String>, ApiError> {
    let mut stmt = db.prepare(sql).map_err(sql_error)?;
    let mut rows = stmt.query([id]).map_err(sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let bytes =
            row.get_ref(0).map_err(sql_error)?.as_str().map_err(|_| {
                ApiError::new(ErrorCode::InvalidState, "invalid history relation ID")
            })?;
        *remaining = remaining
            .checked_sub(bytes.len() + 2)
            .ok_or_else(|| oversized(id))?;
        result.push(bytes.to_owned());
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
