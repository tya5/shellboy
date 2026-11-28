//! Ordered, Thread-owned definitions. Only the conversation driver runs them.
use crate::model::wire_bytes::json_len;
use crate::protocol::{
    ApiError, ContextAddArgs, ContextListArgs, ContextSetArgs, ContextTarget, ErrorCode,
    MAX_FRAME_BYTES, PageCursor, Response,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

fn storage(error: rusqlite::Error) -> ApiError {
    super::storage_error(error.into())
}
fn validate(name: &str, command: Option<&str>, timeout: Option<u32>) -> Result<(), ApiError> {
    if name.trim().is_empty()
        || name.len() > 128
        || name.contains('\0')
        || command.is_some_and(|s| s.trim().is_empty() || !super::schedule::valid_command_field(s))
        || timeout == Some(0)
    {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Expected a nonempty name and command within their bounds, and a positive timeout",
        ));
    }
    Ok(())
}
fn exists(db: &Connection, thread: &str) -> Result<(), ApiError> {
    if !db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
            [thread],
            |r| r.get::<_, bool>(0),
        )
        .map_err(storage)?
    {
        return Err(ApiError::new(ErrorCode::ThreadNotFound, "Thread not found"));
    }
    Ok(())
}
#[derive(Serialize)]
struct ListPage<'a> {
    thread_id: &'a str,
    items: &'a [Value],
    next_cursor: Option<&'a str>,
}

fn serialization_error(_: serde_json::Error) -> ApiError {
    ApiError::new(
        ErrorCode::InvalidState,
        "Cannot serialize context definition page",
    )
}

pub(super) fn list(
    db: &Connection,
    thread: &str,
    args: &ContextListArgs,
    operation_id: &str,
) -> Result<Value, ApiError> {
    const SCOPE: &str = "context.list";
    exists(db, thread)?;
    let after = args
        .cursor
        .as_deref()
        .map(|token| {
            let cursor = PageCursor::decode(token, SCOPE, thread, 0, None)?;
            cursor
                .last_sort_key
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| {
                    ApiError::new(ErrorCode::CursorExpired, "Invalid context list position")
                })
        })
        .transpose()?
        .unwrap_or(0);
    let mut stmt = db.prepare("SELECT id,name,command,timeout_ms FROM context_command WHERE thread_id=?1 AND id>?2 ORDER BY id LIMIT ?3").map_err(storage)?;
    let mut rows = stmt
        .query(params![thread, after, i64::from(args.limit) + 1])
        .map_err(storage)?;
    let frame_overhead = |cursor: Option<&str>| {
        json_len(&Response::success(
            operation_id,
            ListPage {
                thread_id: thread,
                items: &[],
                next_cursor: cursor,
            },
        ))
        .map_err(serialization_error)
    };
    let mut items = Vec::new();
    let mut item_bytes = 0;
    let mut next_cursor = None;
    let mut row = rows.next().map_err(storage)?;
    if row.is_none() && frame_overhead(None)? > MAX_FRAME_BYTES as u64 {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Context list response exceeds the frame limit",
        ));
    }
    while let Some(current) = row {
        let id: i64 = current.get(0).map_err(storage)?;
        let item = json!({
            "name": current.get::<_, String>(1).map_err(storage)?,
            "command": current.get::<_, String>(2).map_err(storage)?,
            "timeout_ms": current.get::<_, u32>(3).map_err(storage)?,
        });
        // Look ahead so a terminal page pays for null, not a continuation.
        row = rows.next().map_err(storage)?;
        let cursor = row
            .is_some()
            .then(|| {
                PageCursor {
                    version: 1,
                    scope: SCOPE.into(),
                    filter: thread.into(),
                    last_sort_key: id.to_string(),
                    watermark: 0,
                    revision: 0,
                    encoding: None,
                }
                .encode()
            })
            .transpose()?;
        // The envelope already includes the brackets; charge each item and
        // just the commas between items, without reserializing the prefix.
        let size = json_len(&item).map_err(serialization_error)? + u64::from(!items.is_empty());
        if frame_overhead(cursor.as_deref())? + item_bytes + size > MAX_FRAME_BYTES as u64 {
            if items.is_empty() {
                return Err(ApiError::new(
                    ErrorCode::InputLimit,
                    "Context definition cannot fit in a response frame",
                ));
            }
            break;
        }
        items.push(item);
        item_bytes += size;
        next_cursor = cursor;
        if items.len() == args.limit as usize {
            break;
        }
    }
    serde_json::to_value(ListPage {
        thread_id: thread,
        items: &items,
        next_cursor: next_cursor.as_deref(),
    })
    .map_err(serialization_error)
}
fn lookup(db: &Connection, thread: &str, name: &str) -> Result<Value, ApiError> {
    db.query_row("SELECT command,timeout_ms FROM context_command WHERE thread_id=?1 AND name=?2",params![thread,name], |r| Ok(json!({"thread_id":thread,"name":name,"command":r.get::<_,String>(0)?,"timeout_ms":r.get::<_,u32>(1)?}))).optional().map_err(storage)?.ok_or_else(||ApiError::new(ErrorCode::TargetUnavailable,"Context command not found in this Thread"))
}
pub(super) fn add(db: &Connection, thread: &str, args: &ContextAddArgs) -> Result<Value, ApiError> {
    validate(&args.name, Some(&args.command), Some(args.timeout_ms))?;
    exists(db, thread)?;
    if db.execute("INSERT INTO context_command(thread_id,name,command,timeout_ms) VALUES(?1,?2,?3,?4) ON CONFLICT(thread_id,name) DO NOTHING", params![thread,args.name,args.command,args.timeout_ms]).map_err(storage)? == 0 {
        return Err(ApiError::new(ErrorCode::OperationConflict,"Context command name already exists in this Thread"));
    }
    lookup(db, thread, &args.name)
}
pub(super) fn get(db: &Connection, thread: &str, args: &ContextTarget) -> Result<Value, ApiError> {
    validate(&args.name, None, None)?;
    exists(db, thread)?;
    lookup(db, thread, &args.name)
}
pub(super) fn set(db: &Connection, thread: &str, args: &ContextSetArgs) -> Result<Value, ApiError> {
    validate(&args.name, args.command.as_deref(), args.timeout_ms)?;
    if args.command.is_none() && args.timeout_ms.is_none() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Specify command or timeout_ms",
        ));
    }
    exists(db, thread)?;
    db.execute("UPDATE context_command SET command=coalesce(?3,command),timeout_ms=coalesce(?4,timeout_ms) WHERE thread_id=?1 AND name=?2",params![thread,args.name,args.command,args.timeout_ms]).map_err(storage)?;
    lookup(db, thread, &args.name)
}
pub(super) fn remove(
    db: &Connection,
    thread: &str,
    args: &ContextTarget,
) -> Result<Value, ApiError> {
    get(db, thread, args)?;
    db.execute(
        "DELETE FROM context_command WHERE thread_id=?1 AND name=?2",
        params![thread, args.name],
    )
    .map_err(storage)?;
    Ok(json!({"accepted":true}))
}
