//! One bounded reservation, consumed at the existing normal-completion boundary.
use crate::{config::Config, protocol::*, storage::StorageError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, ApiError>;
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    text: String,
    set_by: Value,
    operation_id: String,
    set_at: String,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    pending: Option<Pending>,
    consumed_count: u64,
    last_fired_at: Option<String>,
}
fn sql(error: rusqlite::Error) -> ApiError {
    StorageError::from(error).into_api_error()
}
fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, message)
}
fn key(run: &str) -> String {
    format!("revisit.{run}")
}
fn load(db: &Connection, run: &str) -> Result<Option<State>> {
    let saved: Option<String> = db
        .query_row("SELECT value FROM app_meta WHERE key=?1", [key(run)], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sql)?;
    saved
        .map(|body| serde_json::from_str(&body).map_err(|_| invalid("Malformed Revisit state")))
        .transpose()
}
fn save(db: &Connection, run: &str, state: &State) -> Result<()> {
    db.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2) ON CONFLICT(key)DO UPDATE SET value=excluded.value",params![key(run),serde_json::to_string(state).expect("Revisit JSON")]).map_err(sql)?;
    Ok(())
}
fn target(
    db: &Connection,
    requested: Option<&str>,
    caller: Option<&Origin>,
) -> Result<(String, String, Value)> {
    let run = requested
        .or_else(|| caller.map(|origin| origin.run_id.as_str()))
        .ok_or_else(|| invalid("Humans must specify --run for Revisit"))?;
    let (thread, state, owner): (String, String, Option<String>) = db
        .query_row(
            "SELECT thread_id,state,owner_run_id FROM run WHERE id=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql)?
        .ok_or_else(|| ApiError::new(ErrorCode::TargetUnavailable, "Revisit Run not found"))?;
    if state != "live" {
        return Err(invalid("Revisit requires a live Run"));
    }
    let setter = if let Some(caller) = caller {
        let live: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND thread_id=?2 AND state='live')",
                params![caller.run_id, caller.thread_id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        let own = caller.run_id == run && caller.thread_id == thread;
        if !live || (!own && owner.as_deref() != Some(caller.run_id.as_str())) {
            return Err(invalid(
                "Only the Run itself, its live direct owning parent, or a human may operate on Revisit",
            ));
        }
        json!({"kind":"model","run_id":caller.run_id,"thread_id":caller.thread_id,"relation":if own {"self"} else {"owner"}})
    } else {
        json!({"kind":"human"})
    };
    Ok((run.into(), thread, setter))
}
fn view(run: &str, state: &State, config: &Config) -> Value {
    json!({"run_id":run,"pending":state.pending,"consumed_count":state.consumed_count,"last_fired_at":state.last_fired_at,"limit":config.limits.revisit_max_per_run})
}
pub(super) fn get(
    db: &Connection,
    args: &RevisitArgs,
    caller: Option<&Origin>,
    config: &Config,
) -> Result<Value> {
    let (run, _, _) = target(db, args.run_id.as_deref(), caller)?;
    Ok(view(&run, &load(db, &run)?.unwrap_or_default(), config))
}
pub(super) fn set(
    db: &Connection,
    args: &RevisitSetArgs,
    caller: Option<&Origin>,
    operation: &str,
    at: &str,
    config: &Config,
) -> Result<(String, Value)> {
    let (run, thread, setter) = target(db, args.run_id.as_deref(), caller)?;
    if args.text.trim().is_empty() || args.text.len() > MAX_REVISIT_TEXT_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Revisit text must contain 1..8192 UTF-8 bytes and must not be blank",
        ));
    }
    let mut state = load(db, &run)?.unwrap_or_default();
    if config
        .limits
        .revisit_max_per_run
        .is_some_and(|limit| state.consumed_count >= limit)
    {
        return Err(ApiError::new(
            ErrorCode::RevisitLimitReached,
            "This Run has reached its configured Revisit limit",
        ));
    }
    crate::gc::require_capacity_tx(db, config)?;
    let previous = state.pending.take();
    let replaced_other = previous.as_ref().is_some_and(|old| old.set_by != setter);
    let preview = previous
        .as_ref()
        .map(|old| old.text.chars().take(160).collect::<String>());
    state.pending = Some(Pending {
        text: args.text.clone(),
        set_by: setter,
        operation_id: operation.into(),
        set_at: at.into(),
    });
    save(db, &run, &state)?;
    changed(db, &thread, at)?;
    let mut result = view(&run, &state, config);
    result["thread_id"] = json!(thread);
    result["replaced_other"] = json!(replaced_other);
    result["previous_text_preview"] = json!(preview);
    Ok((thread, result))
}
pub(super) fn cancel(
    db: &Connection,
    args: &RevisitArgs,
    caller: Option<&Origin>,
    at: &str,
    config: &Config,
) -> Result<(String, Value)> {
    let (run, thread, _) = target(db, args.run_id.as_deref(), caller)?;
    let mut state = load(db, &run)?.unwrap_or_default();
    let cancelled = state.pending.take().is_some();
    if cancelled {
        save(db, &run, &state)?;
        changed(db, &thread, at)?;
    }
    let mut result = view(&run, &state, config);
    result["cancelled"] = json!(cancelled);
    result["thread_id"] = json!(thread);
    Ok((thread, result))
}
fn changed(db: &Connection, thread: &str, at: &str) -> Result<()> {
    db.execute(
        "UPDATE thread SET revision=revision+1,updated_at=?2 WHERE id=?1",
        params![thread, at],
    )
    .map_err(sql)?;
    Ok(())
}
pub(crate) fn summary(db: &Connection, run: &str) -> Result<Option<RevisitSummary>> {
    Ok(load(db, run)?.map(|state| RevisitSummary {
        pending: state.pending.is_some(),
        consumed_count: state.consumed_count,
        last_fired_at: state.last_fired_at,
    }))
}
/// Called only after idle/owned-work checks, in the same completion transaction.
pub(super) fn fire(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    at: &str,
    config: &Config,
) -> Result<bool> {
    let Some(mut state) = load(tx, run)? else {
        return Ok(false);
    };
    let Some(pending) = state.pending.take() else {
        return Ok(false);
    };
    crate::gc::require_capacity_tx(tx, config)?;
    state.consumed_count = state
        .consumed_count
        .checked_add(1)
        .ok_or_else(|| invalid("Revisit count exceeds its storage range"))?;
    state.last_fired_at = Some(at.into());
    let event = uuid::Uuid::new_v4().to_string();
    let payload = json!({"run_id":run,"instruction":pending.text,"set_by":pending.set_by,"set_operation_id":pending.operation_id,"set_at":pending.set_at,"consumed_count":state.consumed_count,"fired_at":at});
    tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,target_run_id,payload_ref_json,created_at)VALUES(?1,'revisit',?2,?3,?4,?5,?6)",params![event,json!({"kind":"shellboy","operation_id":pending.operation_id,"set_by":pending.set_by}).to_string(),thread,run,payload.to_string(),at]).map_err(sql)?;
    tx.execute(
        "INSERT INTO delivery(event_id,recipient_id,state)VALUES(?1,?2,'pending')",
        params![event, run],
    )
    .map_err(sql)?;
    save(tx, run, &state)?;
    changed(tx, thread, at)?;
    Ok(true)
}
pub(super) fn drop_pending(db: &Connection, run: &str) -> Result<()> {
    if let Some(mut state) = load(db, run)?
        && state.pending.take().is_some()
    {
        save(db, run, &state)?;
    }
    Ok(())
}
pub(super) fn drop_all_pending(db: &Connection) -> std::result::Result<(), StorageError> {
    db.execute("UPDATE app_meta SET value=json_set(value,'$.pending',NULL) WHERE key GLOB 'revisit.*' AND json_type(value,'$.pending')='object'",[])?;
    Ok(())
}
pub(super) fn delete_thread(db: &Connection, thread: &str) -> Result<()> {
    db.execute(
        "DELETE FROM app_meta WHERE key IN (SELECT 'revisit.'||id FROM run WHERE thread_id=?1)",
        [thread],
    )
    .map_err(sql)?;
    Ok(())
}
