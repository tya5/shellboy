//! Bounded saved-fact observations; no receipts, provider calls or source writes.
use crate::protocol::{ApiError, CompactListArgs, ErrorCode, ShellListArgs};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
mod compact;
mod shell;
mod time;
pub(crate) use compact::list as compactions;
pub(crate) use shell::list as shells;
pub(crate) use time::Window;
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}
fn error(e: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("saved readback failed: {e}"))
}
fn limit(n: u32) -> Result<(), ApiError> {
    if (1..=200).contains(&n) {
        Ok(())
    } else {
        Err(invalid("limit must be in 1..=200"))
    }
}
impl CompactListArgs {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        limit(self.limit)?;
        Window::new(self.since.as_deref(), self.until.as_deref())?;
        if self
            .thread_id
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 1024)
        {
            return Err(invalid("invalid thread ID"));
        }
        Ok(())
    }
}
impl ShellListArgs {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        limit(self.limit)?;
        Window::new(self.since.as_deref(), self.until.as_deref())?;
        if self
            .thread_id
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 1024)
            || self
                .command_contains
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 512)
            || self.cursor.as_ref().is_some_and(|s| s.len() > 8192)
        {
            return Err(invalid("invalid or excessive shell list filter"));
        }
        if self.state.as_deref().is_some_and(|s| {
            ![
                "starting", "running", "stopping", "exited", "failed", "stopped",
            ]
            .contains(&s)
        }) {
            return Err(invalid("unknown shell state"));
        }
        Ok(())
    }
}

pub(crate) fn register(db: &Connection) -> Result<(), ApiError> {
    crate::usage_totals::register(db).map_err(error)?;
    let flags = rusqlite::functions::FunctionFlags::SQLITE_UTF8
        | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC;
    db.create_scalar_function("readback_chars", 1, flags, |ctx| {
        Ok(ctx
            .get_raw(0)
            .as_str()
            .ok()
            .map(|s| s.chars().count() as i64))
    })
    .map_err(error)?;
    db.create_scalar_function("readback_prefix", 1, flags, |ctx| {
        Ok(ctx
            .get_raw(0)
            .as_str()
            .ok()
            .map(|s| s.chars().take(8192).collect::<String>()))
    })
    .map_err(error)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
