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
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AdmissionOutcome {
    Inserted,
    OldUnlinked,
    SameAdmission,
    Conflict,
}
pub(super) fn admit_notification_tx(
    tx: &Transaction<'_>,
    thread: &str,
    run: &str,
    turn: &str,
    event: &str,
    payload: &str,
    at: &str,
) -> Result<AdmissionOutcome, crate::storage::StorageError> {
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
        let Some(link) = link else {
            return Ok(AdmissionOutcome::OldUnlinked);
        };
        if link != event {
            return Ok(AdmissionOutcome::Conflict);
        }
        let same:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM node n JOIN turn u ON u.id=n.turn_id WHERE n.id=?1 AND n.kind='event' AND u.thread_id=?2 AND u.run_id=?3 AND u.id=?4)",params![node,thread,run,turn],|r|r.get(0))?;
        // A later run may receive the same event. Preserve the original admission
        // without failing unrelated turn work or changing its creator witness.
        return Ok(if same {
            AdmissionOutcome::SameAdmission
        } else {
            AdmissionOutcome::Conflict
        });
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
    Ok(AdmissionOutcome::Inserted)
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
    fn canonical_admission_is_atomic_and_shared_fork_keeps_creator_marker() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('thread','/','','p','m','now'),('fork','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('run','thread','completed','now'),('retry','thread','completed','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('turn','thread','run','completed','now'),('next','thread','retry','completed','now'); INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,semantic_json,created_at) VALUES('e','context_changed','{}','thread','{}','{\"kind\":\"turn_started\"}','now'); INSERT INTO delivery(event_id,recipient_id,state) VALUES('e','run','pending'),('e','retry','pending');").unwrap();
        {
            let tx = db.transaction().unwrap();
            assert_eq!(
                admit_notification_tx(&tx, "thread", "run", "turn", "e", "synthetic", "now")
                    .unwrap(),
                AdmissionOutcome::Inserted
            );
            tx.rollback().unwrap();
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM node WHERE id='event-e'", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let tx = db.transaction().unwrap();
        assert_eq!(
            admit_notification_tx(&tx, "thread", "run", "turn", "e", "synthetic", "now").unwrap(),
            AdmissionOutcome::Inserted
        );
        tx.commit().unwrap();
        db.execute(
            "INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('fork',0,'event-e')",
            [],
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        assert_eq!(
            admit_notification_tx(&tx, "thread", "retry", "next", "e", "synthetic", "now").unwrap(),
            AdmissionOutcome::Conflict
        );
        tx.commit().unwrap();
        let saved: (String, String) = db
            .query_row(
                "SELECT notification_event_id,turn_id FROM node WHERE id='event-e'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(saved, ("e".into(), "turn".into()));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM thread_path WHERE node_id='event-e'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
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
    fn redelivery_skips_conflict_without_reassigning_marker_or_failing_run() {
        let mut db = fixture();
        db.execute_batch("CREATE TABLE node(id TEXT PRIMARY KEY,notification_event_id TEXT,turn_id TEXT,kind TEXT); INSERT INTO node VALUES('event-e','e','turn','event'); INSERT INTO turn VALUES('next-turn','thread','next-run'); INSERT INTO delivery VALUES('e','next-run','pending');").unwrap();
        let tx = db.transaction().unwrap();
        assert_eq!(
            admit_notification_tx(
                &tx,
                "thread",
                "next-run",
                "next-turn",
                "e",
                "private",
                "now"
            )
            .unwrap(),
            AdmissionOutcome::Conflict
        );
        assert_eq!(
            admit_notification_tx(&tx, "thread", "run", "turn", "e", "private", "now").unwrap(),
            AdmissionOutcome::SameAdmission
        );
        let original: (String, String) = tx
            .query_row(
                "SELECT notification_event_id,turn_id FROM node WHERE id='event-e'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(original, ("e".into(), "turn".into()));
        tx.execute("UPDATE node SET notification_event_id=NULL", [])
            .unwrap();
        assert_eq!(
            admit_notification_tx(
                &tx,
                "thread",
                "next-run",
                "next-turn",
                "e",
                "private",
                "now"
            )
            .unwrap(),
            AdmissionOutcome::OldUnlinked
        );
        assert!(
            tx.query_row("SELECT notification_event_id IS NULL FROM node", [], |r| {
                r.get::<_, bool>(0)
            })
            .unwrap()
        );
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
