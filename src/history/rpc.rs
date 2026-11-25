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
) -> Result<Value, ApiError> {
    let (body, parts) = read_node(db, args)?;
    let mut value = serde_json::to_value(body)
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "history body is not serializable"))?;
    if let (Some(parts), Some(object)) = (parts, value.as_object_mut()) {
        object.insert("parts".into(), parts);
    }
    finish("history.read", operation, value)
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
