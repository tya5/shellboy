//! Bounded keyset scan over one Thread's Turns and each Turn's node IDs.
use crate::{
    protocol::{
        ApiError, ErrorCode, HistoryReceivedArgs, HistoryReceivedPage, HistoryReceivedRow,
        PageCursor,
    },
    storage::StorageError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const TURNS: usize = 32;
const NODES: usize = 200;
const ROWS: usize = 32;
const SOURCE_BYTES: i64 = 64 * 1024;
const SCOPE: &str = "history.received";

#[derive(Clone, Serialize, Deserialize)]
struct Position {
    turn_at: String,
    turn_rowid: i64,
    turn_id: String,
    node_id: Option<String>,
}

fn expired() -> ApiError {
    ApiError::new(
        ErrorCode::CursorExpired,
        "Invalid received history position",
    )
}

fn encode(thread: &str, position: &Position) -> Result<String, ApiError> {
    PageCursor {
        version: 1,
        scope: SCOPE.into(),
        filter: thread.into(),
        last_sort_key: serde_json::to_string(position).map_err(|_| expired())?,
        watermark: 0,
        revision: 0,
        encoding: None,
    }
    .encode()
}

struct Turn {
    id: String,
    position: Position,
    state: String,
}

fn turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    let id: String = row.get(0)?;
    Ok(Turn {
        id: id.clone(),
        position: Position {
            turn_at: row.get(1)?,
            turn_rowid: row.get(2)?,
            turn_id: id,
            node_id: None,
        },
        state: row.get(3)?,
    })
}

pub(crate) fn read(
    db: &Connection,
    args: &HistoryReceivedArgs,
) -> Result<HistoryReceivedPage, StorageError> {
    let thread = &args.thread_id;
    let live: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [thread],
        |r| r.get(0),
    )?;
    if !live {
        return Err(
            ApiError::new(ErrorCode::TargetUnavailable, "Saved Thread is unavailable").into(),
        );
    }
    let position = args
        .cursor
        .as_ref()
        .map(|token| {
            let cursor = PageCursor::decode(token, SCOPE, thread, 0, None)?;
            let position: Position =
                serde_json::from_str(&cursor.last_sort_key).map_err(|_| expired())?;
            if position.turn_at.len() > 128
                || position.turn_id.len() > 512
                || position.node_id.as_ref().is_some_and(|id| id.len() > 512)
            {
                return Err(expired());
            }
            Ok::<_, ApiError>(position)
        })
        .transpose()?;
    // Split the timestamp tie and older ranges so both are indexed seeks,
    // rather than asking SQLite to filter a disjunction over the Thread.
    const COLUMNS: &str = "SELECT CASE WHEN octet_length(id)<=512 THEN id END,CASE WHEN octet_length(created_at)<=128 THEN created_at END,rowid,state FROM turn INDEXED BY turns_by_thread WHERE thread_id=?1";
    let mut turns = Vec::new();
    if let Some(p) = &position {
        let ties = format!("{COLUMNS} AND created_at=?2 AND rowid<?3 ORDER BY rowid DESC LIMIT ?4");
        // A partial Turn is resumed only when its exact row still belongs to this Thread.
        if p.node_id.is_some() {
            let sql = format!("{COLUMNS} AND created_at=?2 AND rowid=?3");
            let current = db
                .query_row(&sql, params![thread, p.turn_at, p.turn_rowid], turn)
                .optional()?
                .ok_or_else(expired)?;
            if current.id != p.turn_id {
                return Err(expired().into());
            }
            turns.push(current);
        }
        let budget = TURNS + 1 - turns.len();
        turns.extend(
            db.prepare(&ties)?
                .query_map(
                    params![thread, p.turn_at, p.turn_rowid, budget as i64],
                    turn,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        if turns.len() <= TURNS {
            let older =
                format!("{COLUMNS} AND created_at<?2 ORDER BY created_at DESC,rowid DESC LIMIT ?3");
            let budget = TURNS + 1 - turns.len();
            turns.extend(
                db.prepare(&older)?
                    .query_map(params![thread, p.turn_at, budget as i64], turn)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
    } else {
        let sql = format!("{COLUMNS} ORDER BY created_at DESC,rowid DESC LIMIT ?2");
        turns = db
            .prepare(&sql)?
            .query_map(params![thread, (TURNS + 1) as i64], turn)?
            .collect::<rusqlite::Result<_>>()?;
    }
    let mut result = HistoryReceivedPage {
        items: Vec::new(),
        next_cursor: None,
    };
    let mut nodes_left = NODES;
    let mut last = None;
    'turns: for (index, mut turn) in turns.into_iter().enumerate() {
        if index == TURNS || nodes_left == 0 || result.items.len() == ROWS {
            result.next_cursor = last.as_ref().map(|p| encode(thread, p)).transpose()?;
            break;
        }
        let lower = position
            .as_ref()
            .filter(|p| p.turn_rowid == turn.position.turn_rowid)
            .and_then(|p| p.node_id.as_deref());
        const NODE: &str = "SELECT CASE WHEN octet_length(id)<=512 THEN id END,kind IN ('incomplete_response','model_stream_observation') AND deleted_reason IS NULL FROM node INDEXED BY node_turn WHERE turn_id=?1";
        let candidates = if let Some(lower) = lower {
            db.prepare(&format!("{NODE} AND id>?2 ORDER BY id LIMIT ?3"))?
                .query_map(params![turn.id, lower, (nodes_left + 1) as i64], candidate)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            db.prepare(&format!("{NODE} ORDER BY id LIMIT ?2"))?
                .query_map(params![turn.id, (nodes_left + 1) as i64], candidate)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, is_received) in candidates {
            if nodes_left == 0 || result.items.len() == ROWS {
                result.next_cursor = last.as_ref().map(|p| encode(thread, p)).transpose()?;
                break 'turns;
            }
            nodes_left -= 1;
            if is_received {
                let (kind, at, excerpt, excerpt_limited) = db.query_row(
                    "SELECT kind,CASE WHEN octet_length(created_at)<=128 THEN created_at ELSE '' END,CASE WHEN octet_length(payload_json)<=?2 THEN substr(json_extract(payload_json,'$.partial_text'),1,80) END,COALESCE(octet_length(payload_json)>?2,0) FROM node WHERE id=?1", params![id,SOURCE_BYTES],
                    |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                result.items.push(HistoryReceivedRow {
                    node_id: id.clone(),
                    turn_id: turn.id.clone(),
                    kind,
                    at,
                    state: turn.state.clone(),
                    request: None,
                    request_limited: false,
                    excerpt,
                    excerpt_limited,
                });
            }
            turn.position.node_id = Some(id);
            last = Some(turn.position.clone());
        }
        turn.position.node_id = None;
        last = Some(turn.position);
    }
    populate_requests(db, thread, &mut result.items)?;
    Ok(result)
}

fn candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, bool)> {
    Ok((row.get(0)?, row.get(1)?))
}

fn populate_requests(
    db: &Connection,
    thread: &str,
    items: &mut [HistoryReceivedRow],
) -> rusqlite::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    // Merge the four ordered ranges of the existing covering index. Fetch
    // receipt rows only for the global chronological prefix, not its sentinel.
    let keys = db.prepare(r#"
        SELECT created_at, rowid AS receipt_row FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='accepted'
        UNION ALL SELECT created_at, rowid FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='held'
        UNION ALL SELECT created_at, rowid FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='applied'
        UNION ALL SELECT created_at, rowid FROM receipt INDEXED BY receipt_target WHERE target_thread_id=?1 AND state='cancelled'
        ORDER BY created_at, receipt_row LIMIT 129
    "#)?
        .query_map([thread], |r| r.get::<_, i64>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = keys.len() > 128;
    let mut receipt = db.prepare("SELECT CASE WHEN octet_length(turn_id)<=512 THEN turn_id END, CASE WHEN octet_length(message_node_id)<=512 THEN message_node_id END FROM receipt WHERE rowid=?1")?;
    let mut excerpts = items
        .iter()
        .map(|row| (row.turn_id.clone(), None))
        .collect::<BTreeMap<_, _>>();
    for key in keys.into_iter().take(128) {
        let (turn, message): (Option<String>, Option<String>) =
            receipt.query_row([key], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if let Some(excerpt @ None) = turn.and_then(|turn| excerpts.get_mut(&turn)) {
            // A known tombstone is skipped. If the earlier identity cannot be
            // read, do not substitute a later request for this Turn.
            *excerpt = match message {
                Some(message) => request_excerpt(db, &message)?,
                None => Some((None, true)),
            };
        }
    }
    for row in items {
        if let Some(Some((excerpt, limited))) = excerpts.get(&row.turn_id) {
            row.request.clone_from(excerpt);
            row.request_limited = *limited;
        } else {
            row.request_limited = has_more;
        }
    }
    Ok(())
}

fn request_excerpt(
    db: &Connection,
    node: &str,
) -> rusqlite::Result<Option<(Option<String>, bool)>> {
    db.query_row(r#"SELECT CASE WHEN octet_length(payload_json)<=?2 THEN
      substr(coalesce((SELECT json_extract(value,'$.text') FROM json_each(payload_json,'$.content')
        WHERE CASE WHEN type='object' THEN json_extract(value,'$.type') IN ('input_text','text')
        AND json_type(value,'$.text')='text' END LIMIT 1),
        CASE WHEN json_type(payload_json,'$.text')='text' THEN json_extract(payload_json,'$.text') END),1,80) END,
      COALESCE(octet_length(payload_json)>?2,0)
      FROM node WHERE id=?1 AND deleted_reason IS NULL"#,
        params![node, SOURCE_BYTES], |r| Ok((r.get(0)?, r.get(1)?))).optional()
}
