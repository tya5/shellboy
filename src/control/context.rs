//! Append-only context observations. They never create a Turn or wake a wait.
use rusqlite::{Transaction, params};
use serde_json::{Value, json};

pub(crate) fn record(
    tx: &Transaction<'_>,
    thread: &str,
    kind: &str,
    actor: Value,
    change: Value,
    at: &str,
) -> rusqlite::Result<()> {
    tx.execute("INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at) VALUES(?1,'context_changed',?2,?3,?4,?5)", params![uuid::Uuid::new_v4().to_string(),actor.to_string(),thread,json!({"kind":kind,"actor":actor,"change":change,"observed_at":at,"timezone":chrono::Local::now().offset().to_string()}).to_string(),at])?;
    Ok(())
}
pub(super) fn turn_started(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    at: &str,
) -> rusqlite::Result<()> {
    let previous: Option<String> = tx.query_row(
        "SELECT max(ended_at) FROM turn WHERE thread_id=?1 AND id<>?2 AND ended_at IS NOT NULL",
        params![thread, turn],
        |row| row.get(0),
    )?;
    record(
        tx,
        thread,
        "turn_started",
        json!({"kind":"shellboy"}),
        json!({"run_id":run,"turn_id":turn,"previous_turn_ended_at":previous}),
        at,
    )
}
