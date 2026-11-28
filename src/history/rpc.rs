//! Public history result assembly over the caller's existing DB snapshot.
use super::{list_nodes, message_budget, read_node, window};
use crate::protocol::{
    ApiError, ErrorCode, HistoryListArgs, HistoryReadArgs, HistoryWindowArgs, Response,
};
use rusqlite::Connection;
use serde_json::Value;

pub(crate) fn read(
    db: &Connection,
    args: &HistoryReadArgs,
    operation: &str,
    next_sequence: impl FnOnce() -> u64,
) -> Result<Value, ApiError> {
    if let Some(ids) = &args.node_ids {
        return finish(
            "history.read",
            operation,
            serialize(observe_nodes(db, ids, next_sequence())?)?,
        );
    }
    let (mut body, parts) = read_node(db, args)?;
    if args.display.is_none() && !args.parts && !args.end_message_only && args.cursor.is_none() {
        body.observation = Some(observe_nodes(
            db,
            std::slice::from_ref(&args.node_id),
            next_sequence(),
        )?);
    }
    let mut value = serde_json::to_value(body)
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "history body is not serializable"))?;
    if let (Some(parts), Some(object)) = (parts, value.as_object_mut()) {
        object.insert("parts".into(), parts);
    }
    finish("history.read", operation, value)
}

fn observe_nodes(
    db: &Connection,
    ids: &[String],
    read_sequence: u64,
) -> Result<crate::protocol::HistoryNodeSelection, ApiError> {
    use crate::protocol::{
        HistoryNodeAvailability as State, HistoryNodeObservation, HistoryNodeSelection,
    };
    use rusqlite::OptionalExtension;
    let mut items = Vec::with_capacity(ids.len());
    let mut query = db.prepare(
        "SELECT payload_revision,deleted_reason IS NOT NULL OR payload_json IS NULL FROM node WHERE id=?1",
    ).map_err(super::sql_error)?;
    for id in ids {
        let saved: Option<(i64, bool)> = query
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(super::sql_error)?;
        let availability = match saved {
            None | Some((_, true)) => State::Tombstoned,
            Some((_, false)) => {
                if super::reachability::searchable(db, id).map_err(super::sql_error)? {
                    State::Available
                } else if super::reachability::released_by_prune(db, id)
                    .map_err(super::sql_error)?
                {
                    State::Tombstoned
                } else {
                    State::ThreadDeleted
                }
            }
        };
        items.push(HistoryNodeObservation {
            node_id: id.clone(),
            availability,
            source_revision: saved.map(|(revision, _)| revision),
            tools: Vec::new(),
            tools_truncated: false,
            notice: None,
        });
    }
    for chunk in ids.chunks(super::window_facts::tools::MAX_NODES) {
        let facts = super::window_facts::tools::read_nodes_tx(db, chunk)
            .map_err(crate::storage::StorageError::into_api_error)?
            .into_window();
        let rows = &mut items[..];
        for call in facts.calls {
            if let Some(row) = rows.iter_mut().find(|row| row.node_id == call.node_id) {
                if row.tools.len() < 16 {
                    row.tools.push(call);
                } else {
                    row.tools_truncated = true;
                }
            }
        }
        for row in rows.iter_mut() {
            row.tools_truncated |= facts.limited_node_ids.contains(&row.node_id);
        }
        for notice in facts.notices {
            if let Some(row) = rows.iter_mut().find(|row| row.node_id == notice.node_id) {
                row.notice = Some(notice);
            }
        }
    }
    Ok(HistoryNodeSelection {
        read_sequence,
        observed_at: chrono::Utc::now().to_rfc3339(),
        items,
    })
}

pub(crate) fn list(
    db: &Connection,
    args: &HistoryListArgs,
    operation: &str,
) -> Result<Value, ApiError> {
    finish("history.list", operation, serialize(list_nodes(db, args)?)?)
}

pub(crate) fn selected_window(
    db: &Connection,
    args: &HistoryWindowArgs,
    operation: &str,
) -> Result<Value, ApiError> {
    finish("history.window", operation, serialize(window(db, args)?)?)
}

fn serialize(result: impl serde::Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(result).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidState,
            "History result serialization failed",
        )
    })
}

pub(crate) fn finish(command: &str, operation: &str, result: Value) -> Result<Value, ApiError> {
    let mut response = Response::success(operation, result);
    fit_response(command, &mut response)
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "History provenance budget failed"))?;
    Ok(response.result.expect("success response owns its result"))
}

/// Also used on the final envelope, including replayed results.
pub(crate) fn fit_response(
    command: &str,
    response: &mut Response<Value>,
) -> Result<(), serde_json::Error> {
    let slots = match command {
        "history.read" => message_budget::Slots::Body,
        "history.list" | "history.window" => message_budget::Slots::Nodes,
        _ => return Ok(()),
    };
    message_budget::fit(response, slots)
}
