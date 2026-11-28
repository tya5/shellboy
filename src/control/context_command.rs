//! Ordered, Thread-owned definitions. Only the conversation driver runs them.
use crate::protocol::{ApiError, ContextAddArgs, ContextSetArgs, ContextTarget, ErrorCode};
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
pub(super) fn list(db: &Connection, thread: &str) -> Result<Value, ApiError> {
    exists(db, thread)?;
    Ok(json!({"thread_id":thread,"items":definitions(db,thread)?}))
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
