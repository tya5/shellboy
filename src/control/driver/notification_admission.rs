//! Canonical notification admission. Generic history append has no marker argument.
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

pub(super) fn admit_notification_tx(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    event: &str,
    payload: &str,
    at: &str,
) -> Result<(), crate::storage::StorageError> {
    let node = format!("event-{event}");
    let existing: Option<Option<String>> = tx
        .query_row(
            "SELECT notification_event_id FROM node WHERE id=?1",
            [&node],
            |r| r.get(0),
        )
        .optional()?;
    // Old unlinked nodes remain unlinked; retries never reassign authority.
    if let Some(link) = existing {
        if link.as_deref().is_some_and(|link| link != event) {
            return Err(crate::storage::StorageError::Schema(
                "notification admission conflict".into(),
            ));
        }
        return Ok(());
    }
    let valid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM event e JOIN delivery d ON d.event_id=e.id JOIN turn u ON u.id=?4 WHERE e.id=?1 AND e.target_thread_id=?2 AND u.thread_id=?2 AND u.run_id=?3 AND d.recipient_id=?3 AND d.state='pending' AND (e.target_run_id IS NULL OR e.target_run_id=?3))",
        params![event,thread,run,turn],|r|r.get(0)
    )?;
    if !valid {
        return Err(crate::storage::StorageError::Schema(
            "notification admission context mismatch".into(),
        ));
    }
    crate::history::append_node(
        tx,
        thread,
        &node,
        Some(turn),
        "event",
        &json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {payload}")}],"event_id":event}),
        at,
    )?;
    tx.execute(
        "UPDATE node SET notification_event_id=?2 WHERE id=?1 AND notification_event_id IS NULL",
        params![node, event],
    )?;
    Ok(())
}
