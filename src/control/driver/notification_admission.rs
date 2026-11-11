//! Canonical notification admission. Generic history append has no marker argument.
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

struct PendingNotification<'a> {
    thread: &'a str,
    run: &'a str,
    turn: &'a str,
    event: &'a str,
}
impl<'a> PendingNotification<'a> {
    fn validate(
        tx: &Transaction<'_>,
        thread: &'a str,
        run: &'a str,
        turn: &'a str,
        event: &'a str,
    ) -> Result<Self, crate::storage::StorageError> {
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM event e JOIN delivery d ON d.event_id=e.id JOIN turn u ON u.id=?4 WHERE e.id=?1 AND e.target_thread_id=?2 AND u.thread_id=?2 AND u.run_id=?3 AND d.recipient_id=?3 AND d.state='pending' AND (e.target_run_id IS NULL OR e.target_run_id=?3))",params![event,thread,run,turn],|r|r.get(0))?;
        if !valid {
            return Err(crate::storage::StorageError::Schema(
                "notification admission context mismatch".into(),
            ));
        }
        Ok(Self {
            thread,
            run,
            turn,
            event,
        })
    }
}
pub(super) fn admit_notification_tx(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    event: &str,
    payload: &str,
    at: &str,
) -> Result<(), crate::storage::StorageError> {
    let pending = PendingNotification::validate(tx, thread, run, turn, event)?;
    let (thread, run, turn, event) = (pending.thread, pending.run, pending.turn, pending.event);
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
        if link.is_some() {
            let same:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM node n JOIN turn u ON u.id=n.turn_id WHERE n.id=?1 AND n.kind='event' AND u.thread_id=?2 AND u.run_id=?3 AND u.id=?4)",params![node,thread,run,turn],|r|r.get(0))?;
            if !same {
                return Err(crate::storage::StorageError::Schema(
                    "notification creator reuse conflict".into(),
                ));
            }
        }
        return Ok(());
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

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE event(id TEXT PRIMARY KEY,target_thread_id TEXT,target_run_id TEXT); CREATE TABLE delivery(event_id TEXT,recipient_id TEXT,state TEXT); CREATE TABLE turn(id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT); INSERT INTO event VALUES('e','thread',NULL); INSERT INTO delivery VALUES('e','run','pending'); INSERT INTO turn VALUES('turn','thread','run'); INSERT INTO turn VALUES('foreign-turn','foreign','foreign-run');").unwrap();
        db
    }
    #[test]
    fn pending_constructor_rejects_wrong_creator_and_missing_delivery() {
        let mut db = fixture();
        let tx = db.transaction().unwrap();
        assert!(PendingNotification::validate(&tx, "thread", "run", "turn", "e").is_ok());
        assert!(PendingNotification::validate(&tx, "thread", "run", "foreign-turn", "e").is_err());
        tx.execute("DELETE FROM delivery", []).unwrap();
        assert!(PendingNotification::validate(&tx, "thread", "run", "turn", "e").is_err());
    }
    #[test]
    fn pending_constructor_rejects_other_target_run() {
        let mut db = fixture();
        let tx = db.transaction().unwrap();
        tx.execute("UPDATE event SET target_run_id='other'", [])
            .unwrap();
        assert!(PendingNotification::validate(&tx, "thread", "run", "turn", "e").is_err());
    }
}
