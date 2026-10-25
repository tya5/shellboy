//! Read-only projections of recorded lifecycle and ownership information.
use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{ApiError, ErrorCode};

/// Missing fields mean unrecorded or inapplicable, never inferred execution facts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContext {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Closed wall-clock lifetime, including preparation and waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_reason: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tombstone_reason: Option<String>,
}

type Result<T> = std::result::Result<T, ApiError>;

fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(format!("context database read failed: {error}"))
}
fn missing() -> ApiError {
    ApiError::new(ErrorCode::TargetUnavailable, "context target not found")
}
fn lifetime(created: &str, ended: Option<&str>) -> Option<u64> {
    let start = DateTime::parse_from_rfc3339(created).ok()?;
    let end = DateTime::parse_from_rfc3339(ended?).ok()?;
    if end < start {
        return None;
    }
    u64::try_from(end.signed_duration_since(start).num_milliseconds()).ok()
}
fn reason(raw: Option<String>) -> Result<Option<Value>> {
    let parsed = raw
        .map(|raw| {
            serde_json::from_str::<Value>(&raw)
                .map_err(|_| ApiError::new(ErrorCode::InvalidState, "invalid saved context reason"))
        })
        .transpose()?;
    Ok(parsed.filter(|value| !value.is_null()))
}

pub fn for_thread(db: &Connection, id: &str) -> Result<ExecutionContext> {
    db.query_row(
        "SELECT parent_thread_id,updated_at,deleted_at FROM thread WHERE id=?1",
        [id],
        |row| {
            let deleted: Option<String> = row.get(2)?;
            Ok(ExecutionContext {
                state: if deleted.is_some() {
                    "deleted"
                } else {
                    "active"
                }
                .into(),
                parent_thread_id: row.get(0)?,
                updated_at: row.get(1)?,
                thread_id: Some(id.into()),
                ..Default::default()
            })
        },
    )
    .optional()
    .map_err(sql_error)?
    .ok_or_else(missing)
}

pub fn for_run(db: &Connection, id: &str) -> Result<ExecutionContext> {
    let mut context=db.query_row("SELECT thread_id,owner_run_id,state,created_at,ended_at,stop_reason FROM run WHERE id=?1",[id],|row| Ok(ExecutionContext{thread_id:row.get(0)?,owner_run_id:row.get(1)?,state:row.get(2)?,created_at:row.get(3)?,ended_at:row.get(4)?,stop_reason:row.get(5)?,run_id:Some(id.into()),..Default::default()})).optional().map_err(sql_error)?.ok_or_else(missing)?;
    context.lifetime_ms = lifetime(
        context.created_at.as_deref().unwrap_or_default(),
        context.ended_at.as_deref(),
    );
    let latest:Option<(String,Option<String>)>=db.query_row("SELECT id,recovery_reason_json FROM turn WHERE run_id=?1 ORDER BY created_at DESC,id DESC LIMIT 1",[id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(sql_error)?;
    if let Some((turn, raw)) = latest {
        context.turn_id = Some(turn);
        context.recovery_reason = reason(raw)?;
    }
    Ok(context)
}

pub fn for_turn(db: &Connection, id: &str) -> Result<ExecutionContext> {
    let (mut context,raw)=db.query_row("SELECT t.thread_id,t.run_id,t.state,t.created_at,t.ended_at,t.recovery_reason_json,r.owner_run_id FROM turn t JOIN run r ON r.id=t.run_id WHERE t.id=?1",[id],|row|Ok((ExecutionContext{thread_id:row.get(0)?,run_id:row.get(1)?,state:row.get(2)?,created_at:row.get(3)?,ended_at:row.get(4)?,owner_run_id:row.get(6)?,turn_id:Some(id.into()),..Default::default()},row.get::<_,Option<String>>(5)?))).optional().map_err(sql_error)?.ok_or_else(missing)?;
    context.lifetime_ms = lifetime(
        context.created_at.as_deref().unwrap_or_default(),
        context.ended_at.as_deref(),
    );
    context.recovery_reason = reason(raw)?;
    if context.state == "failed" {
        context.failure_reason = context.recovery_reason.clone();
    }
    Ok(context)
}

pub fn for_message(db: &Connection, id: &str) -> Result<ExecutionContext> {
    db.query_row("SELECT target_thread_id,run_id,turn_id,state,created_at,NULLIF(updated_at,''),hold_reason FROM receipt WHERE message_node_id=?1",[id],|row|Ok(ExecutionContext{thread_id:row.get(0)?,run_id:row.get(1)?,turn_id:row.get(2)?,state:row.get(3)?,created_at:row.get(4)?,updated_at:row.get(5)?,hold_reason:row.get(6)?,node_id:Some(id.into()),..Default::default()})).optional().map_err(sql_error)?.ok_or_else(missing)
}

pub fn for_node(db: &Connection, id: &str) -> Result<ExecutionContext> {
    db.query_row("SELECT n.created_at,n.deleted_reason,n.turn_id,t.run_id,t.thread_id,r.owner_run_id FROM node n LEFT JOIN turn t ON t.id=n.turn_id LEFT JOIN run r ON r.id=t.run_id WHERE n.id=?1",[id],|row|{
        let tombstone_reason:Option<String>=row.get(1)?;
        Ok(ExecutionContext{state:if tombstone_reason.is_some(){"tombstone"}else{"present"}.into(),created_at:row.get(0)?,turn_id:row.get(2)?,run_id:row.get(3)?,thread_id:row.get(4)?,owner_run_id:row.get(5)?,tombstone_reason,node_id:Some(id.into()),..Default::default()})
    }).optional().map_err(sql_error)?.ok_or_else(missing)
}
