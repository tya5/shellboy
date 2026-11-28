//! Ordered, Thread-owned definitions. Only the conversation driver runs them.
use crate::protocol::{
    ApiError, ContextAddArgs, ContextListArgs, ContextSetArgs, ContextTarget, ErrorCode,
    MAX_FRAME_BYTES, PageCursor,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Serialize)]
pub(super) struct Definition {
    pub(super) name: String,
    pub(super) command: String,
    pub(super) timeout_ms: u32,
}
fn storage(error: rusqlite::Error) -> ApiError {
    super::storage_error(error.into())
}
fn validate(name: &str, command: Option<&str>, timeout: Option<u32>) -> Result<(), ApiError> {
    if name.trim().is_empty()
        || name.len() > 128
        || name.contains('\0')
        || command.is_some_and(|s| s.trim().is_empty() || s.len() > 8192 || s.contains('\0'))
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
pub(super) fn definitions(db: &Connection, thread: &str) -> Result<Vec<Definition>, ApiError> {
    let mut stmt = db
        .prepare(
            "SELECT name,command,timeout_ms FROM context_command WHERE thread_id=?1 ORDER BY id",
        )
        .map_err(storage)?;
    stmt.query_map([thread], |r| {
        Ok(Definition {
            name: r.get(0)?,
            command: r.get(1)?,
            timeout_ms: r.get(2)?,
        })
    })
    .map_err(storage)?
    .collect::<Result<_, _>>()
    .map_err(storage)
}
pub(super) fn list(
    db: &Connection,
    thread: &str,
    args: &ContextListArgs,
) -> Result<Value, ApiError> {
    const SCOPE: &str = "context.list";
    // JSON-escaped definitions can exceed the frame before the row limit.
    // Leave room for the RPC envelope and continuation token.
    const ITEM_BYTES: usize = MAX_FRAME_BYTES - 1024 * 1024;
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
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut last = after;
    let mut next_cursor = None;
    while let Some(row) = rows.next().map_err(storage)? {
        let item = json!({
            "name": row.get::<_, String>(1).map_err(storage)?,
            "command": row.get::<_, String>(2).map_err(storage)?,
            "timeout_ms": row.get::<_, u32>(3).map_err(storage)?,
        });
        let size = item.to_string().len() + 1;
        if items.len() == args.limit as usize || bytes + size > ITEM_BYTES {
            next_cursor = Some(
                PageCursor {
                    version: 1,
                    scope: SCOPE.into(),
                    filter: thread.into(),
                    last_sort_key: last.to_string(),
                    watermark: 0,
                    revision: 0,
                    encoding: None,
                }
                .encode()?,
            );
            break;
        }
        bytes += size;
        last = row.get(0).map_err(storage)?;
        items.push(item);
    }
    Ok(json!({"thread_id":thread,"items":items,"next_cursor":next_cursor}))
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
