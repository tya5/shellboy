//! Incremental retention of dead-recipient deliveries and unreferenced shell events.
use rusqlite::{Connection, params};

use crate::storage::StorageError;

// One pass changes at most 64 deliveries plus two rows per event (128 total).
const DELIVERY_BATCH: i64 = 64;
const EVENT_BATCH: i64 = 32;

// Matching TEXT affinity lets the correlated node lookup seek its expression index.
const ELIGIBLE_SQL: &str = "SELECT EXISTS(SELECT 1 FROM event e JOIN run r ON r.id=e.target_run_id WHERE e.rowid=?1 AND e.kind='shell_ended' AND r.state IN ('completed','stopped','failed') AND NOT EXISTS(SELECT 1 FROM node n INDEXED BY node_event_retention WHERE n.kind='event' AND json_valid(n.payload_json) AND CAST(json_extract(n.payload_json,'$.event_id') AS TEXT)=e.id) AND NOT EXISTS(SELECT 1 FROM delivery d INDEXED BY delivery_event_state WHERE d.event_id=e.id AND d.state<'dropped') AND NOT EXISTS(SELECT 1 FROM delivery d INDEXED BY delivery_event_state WHERE d.event_id=e.id AND d.state>'dropped' AND d.state<'processed') AND NOT EXISTS(SELECT 1 FROM delivery d INDEXED BY delivery_event_state WHERE d.event_id=e.id AND d.state>'processed')) ";

#[derive(Clone, Copy, Default)]
pub(super) struct Cursor {
    deliveries: i64,
    events: i64,
}

impl Cursor {
    /// (deliveries, events): which scans just reached the end and restart
    /// from the first row. The two scans wrap independently.
    pub(super) fn wrapped(self) -> (bool, bool) {
        (self.deliveries == 0, self.events == 0)
    }
}

/// Progress of targeted retention for one ended Run.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) enum RunCursor {
    /// Delete the Run's pending deliveries (seek on `delivery_pending`).
    #[default]
    Deliveries,
    /// Visit the Run's shell events after this rowid (`event_run_shell_ended`).
    Events(i64),
    Done,
}

/// One bounded unit of targeted retention for an ended Run (#171 rule 3): only
/// that Run's rows are visited, through indexes, never the whole tables.
pub(super) fn collect_run(
    db: &mut Connection,
    run: &str,
    cursor: RunCursor,
) -> Result<RunCursor, StorageError> {
    let tx = db.transaction()?;
    let terminal: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND state IN ('completed','stopped','failed'))",
        [run],
        |r| r.get(0),
    )?;
    if !terminal {
        return Ok(RunCursor::Done);
    }
    let next = match cursor {
        RunCursor::Deliveries => {
            let removed = tx.execute(
                "DELETE FROM delivery WHERE rowid IN (SELECT rowid FROM delivery INDEXED BY delivery_pending WHERE recipient_id=?1 AND state='pending' LIMIT ?2)",
                params![run, DELIVERY_BATCH],
            )?;
            if removed as i64 == DELIVERY_BATCH {
                RunCursor::Deliveries
            } else {
                RunCursor::Events(0)
            }
        }
        RunCursor::Events(after) => {
            let events = tx
                .prepare("SELECT rowid FROM event INDEXED BY event_run_shell_ended WHERE target_run_id=?1 AND kind='shell_ended' AND rowid>?2 ORDER BY rowid LIMIT ?3")?
                .query_map(params![run, after, EVENT_BATCH], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for row in &events {
                let eligible: bool = tx.query_row(ELIGIBLE_SQL, params![row], |r| r.get(0))?;
                if eligible {
                    // Only dropped/processed deliveries remain for an eligible event.
                    tx.execute(
                        "DELETE FROM delivery WHERE event_id=(SELECT id FROM event WHERE rowid=?1)",
                        [row],
                    )?;
                    tx.execute("DELETE FROM event WHERE rowid=?1", [row])?;
                }
            }
            match events.last() {
                Some(last) if events.len() == EVENT_BATCH as usize => RunCursor::Events(*last),
                _ => RunCursor::Done,
            }
        }
        RunCursor::Done => RunCursor::Done,
    };
    tx.commit()?;
    Ok(next)
}

pub(super) fn collect_batch(db: &mut Connection, cursor: Cursor) -> Result<Cursor, StorageError> {
    let tx = db.transaction()?;
    // Scan by rowid, including non-candidates, so the work is bounded even
    // when all pending deliveries belong to live or recovery-waiting Runs.
    let deliveries = tx
        .prepare("SELECT rowid FROM delivery WHERE rowid>?1 ORDER BY rowid LIMIT ?2")?
        .query_map(params![cursor.deliveries, DELIVERY_BATCH], |r| {
            r.get::<_, i64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for row in &deliveries {
        tx.execute("DELETE FROM delivery WHERE rowid=?1 AND state='pending' AND EXISTS (SELECT 1 FROM run WHERE id=delivery.recipient_id AND state IN ('completed','stopped','failed'))", [row])?;
    }
    let events = tx
        .prepare("SELECT rowid FROM event WHERE rowid>?1 ORDER BY rowid LIMIT ?2")?
        .query_map(params![cursor.events, EVENT_BATCH], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for row in &events {
        // At most one delivery per event per pass; unknown and pending states
        // stay untouched, and any saved history node protects its event.
        let eligible: bool = tx.query_row(ELIGIBLE_SQL, params![row], |r| r.get(0))?;
        if eligible {
            tx.execute("DELETE FROM delivery WHERE rowid=(SELECT d.rowid FROM delivery d WHERE d.event_id=(SELECT id FROM event WHERE rowid=?1) LIMIT 1)", [row])?;
            tx.execute("DELETE FROM event WHERE rowid=?1 AND NOT EXISTS(SELECT 1 FROM delivery WHERE event_id=event.id)", [row])?;
        }
    }
    tx.commit()?;
    Ok(Cursor {
        deliveries: if deliveries.len() < DELIVERY_BATCH as usize {
            0
        } else {
            *deliveries.last().unwrap()
        },
        events: if events.len() < EVENT_BATCH as usize {
            0
        } else {
            *events.last().unwrap()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES
                ('a','/','','p','m','now'),('b','/','','p','m','now'),('c','/','','p','m','now');
            INSERT INTO run(id,thread_id,state,created_at) VALUES
                ('done','a','completed','now'),('live','b','live','now'),('stopping','c','stopping','now');").unwrap();
        db
    }
    fn event(db: &Connection, id: &str, kind: &str, target: &str, date: &str) {
        db.execute("INSERT INTO event(id,kind,source_json,target_thread_id,target_run_id,payload_ref_json,created_at) VALUES(?1,?2,'{}','a',?3,'{}',?4)", params![id,kind,target,date]).unwrap();
    }
    fn delivery(db: &Connection, event: &str, run: &str, state: &str) {
        db.execute(
            "INSERT INTO delivery(event_id,recipient_id,state) VALUES(?1,?2,?3)",
            params![event, run, state],
        )
        .unwrap();
    }
    fn exists(db: &Connection, sql: &str, id: &str) -> bool {
        db.query_row(sql, [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn terminal_delivery_cleanup_preserves_redelivery_and_referenced_or_live_events() {
        let mut db = fixture();
        let old = "2026-01-01T00:00:00Z";
        for (id, kind, run, date) in [
            ("context", "context_changed", "done", old),
            ("old", "shell_ended", "done", old),
            ("protected", "shell_ended", "done", old),
            ("recent", "shell_ended", "done", "2026-10-02T00:00:00Z"),
            ("live_event", "shell_ended", "live", old),
            ("stopping_event", "shell_ended", "stopping", old),
            ("cross_run", "shell_ended", "done", old),
            ("unknown", "shell_ended", "done", old),
            ("bad_date", "shell_ended", "done", "not-a-date"),
            ("revisit", "revisit", "done", old),
        ] {
            event(&db, id, kind, run, date);
        }
        for (id, run, state) in [
            ("context", "done", "pending"),
            ("old", "done", "processed"),
            ("protected", "done", "processed"),
            ("recent", "done", "processed"),
            ("live_event", "live", "pending"),
            ("stopping_event", "stopping", "pending"),
            ("cross_run", "live", "pending"),
            ("unknown", "done", "unexpected"),
            ("bad_date", "done", "processed"),
            ("revisit", "done", "processed"),
        ] {
            delivery(&db, id, run, state);
        }
        db.execute(
            "INSERT INTO node(id,kind,created_at,payload_json) VALUES('ref','event','now',?1)",
            [r#"{"event_id":"protected"}"#],
        )
        .unwrap();
        event(&db, "123", "shell_ended", "done", old);
        delivery(&db, "123", "done", "processed");
        db.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES('numeric-ref','event','now',?1)", [r#"{"event_id":123}"#]).unwrap();
        collect_batch(&mut db, Cursor::default()).unwrap();
        assert!(exists(
            &db,
            "SELECT EXISTS(SELECT 1 FROM event WHERE id=?1)",
            "123"
        ));
        assert!(!exists(
            &db,
            "SELECT EXISTS(SELECT 1 FROM delivery WHERE event_id=?1)",
            "context"
        ));
        assert!(exists(
            &db,
            "SELECT EXISTS(SELECT 1 FROM event WHERE id=?1)",
            "context"
        ));
        assert!(!exists(
            &db,
            "SELECT EXISTS(SELECT 1 FROM event WHERE id=?1)",
            "old"
        ));
        assert!(!exists(
            &db,
            "SELECT EXISTS(SELECT 1 FROM event WHERE id=?1)",
            "recent"
        ));
        assert!(!exists(
            &db,
            "SELECT EXISTS(SELECT 1 FROM event WHERE id=?1)",
            "bad_date"
        ));
        for id in [
            "protected",
            "live_event",
            "stopping_event",
            "cross_run",
            "unknown",
            "revisit",
        ] {
            assert!(
                exists(&db, "SELECT EXISTS(SELECT 1 FROM event WHERE id=?1)", id),
                "lost {id}"
            );
            assert!(
                exists(
                    &db,
                    "SELECT EXISTS(SELECT 1 FROM delivery WHERE event_id=?1)",
                    id
                ),
                "lost delivery {id}"
            );
        }
        // Context was not marked processed: a later live Run can receive it.
        delivery(&db, "context", "live", "pending");
        collect_batch(&mut db, Cursor::default()).unwrap();
        assert!(exists(
            &db,
            "SELECT EXISTS(SELECT 1 FROM delivery WHERE event_id=?1 AND recipient_id='live' AND state='pending')",
            "context"
        ));
    }

    #[test]
    fn scans_and_mutations_are_bounded_and_wrap_to_catch_late_arrivals() {
        let mut db = fixture();
        for n in 0..300 {
            let id = format!("e{n:03}");
            event(&db, &id, "context_changed", "done", "2026-10-02T00:00:00Z");
            delivery(&db, &id, "done", "pending");
        }
        let mut cursor = collect_batch(&mut db, Cursor::default()).unwrap();
        let count = |db: &Connection| {
            db.query_row("SELECT count(*) FROM delivery", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(count(&db), 236);
        for expected in [172, 108, 44, 0] {
            cursor = collect_batch(&mut db, cursor).unwrap();
            assert_eq!(count(&db), expected);
        }
        // Rowid reuse and inserts behind the cursor are reached after wrap.
        delivery(&db, "e000", "done", "pending");
        collect_batch(&mut db, cursor).unwrap();
        assert_eq!(count(&db), 0);
        assert_eq!(
            db.query_row("SELECT count(*) FROM event", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            300
        );
    }

    #[test]
    fn ended_run_retention_touches_only_that_runs_rows_by_index() {
        let mut db = fixture();
        db.execute(
            "INSERT INTO run(id,thread_id,state,created_at) VALUES('other','a','completed','now')",
            [],
        )
        .unwrap();
        for n in 0..40 {
            event(&db, &format!("done-{n}"), "shell_ended", "done", "now");
            delivery(&db, &format!("done-{n}"), "done", "processed");
            event(&db, &format!("other-{n}"), "shell_ended", "other", "now");
            delivery(&db, &format!("other-{n}"), "other", "processed");
            event(&db, &format!("ctx-{n}"), "context_changed", "done", "now");
            delivery(&db, &format!("ctx-{n}"), "done", "pending");
            delivery(&db, &format!("ctx-{n}"), "other", "pending");
        }
        let count =
            |db: &Connection, sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        let mut cursor = RunCursor::default();
        let mut units = 0;
        while cursor != RunCursor::Done {
            cursor = collect_run(&mut db, "done", cursor).unwrap();
            units += 1;
        }
        assert!(
            units <= 4,
            "40 events in 32-row batches plus one delivery unit: {units}"
        );
        assert_eq!(
            count(&db, "SELECT count(*) FROM event WHERE id LIKE 'done-%'"),
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM delivery WHERE recipient_id='done'"
            ),
            0
        );
        // The other ended Run and its pending deliveries are untouched.
        assert_eq!(
            count(&db, "SELECT count(*) FROM event WHERE id LIKE 'other-%'"),
            40
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM delivery WHERE recipient_id='other'"
            ),
            80
        );
        // A live Run is never collected.
        event(&db, "live-0", "shell_ended", "live", "now");
        delivery(&db, "live-0", "live", "pending");
        assert_eq!(
            collect_run(&mut db, "live", RunCursor::default()).unwrap(),
            RunCursor::Done
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM delivery WHERE recipient_id='live'"
            ),
            1
        );
        for sql in [
            "EXPLAIN QUERY PLAN SELECT rowid FROM event INDEXED BY event_run_shell_ended WHERE target_run_id='done' AND kind='shell_ended' AND rowid>0 ORDER BY rowid LIMIT 32",
            "EXPLAIN QUERY PLAN SELECT rowid FROM delivery INDEXED BY delivery_pending WHERE recipient_id='done' AND state='pending' LIMIT 64",
        ] {
            let plan: Vec<String> = db
                .prepare(sql)
                .unwrap()
                .query_map([], |r| r.get(3))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(plan.iter().all(|d| d.starts_with("SEARCH")), "{plan:?}");
        }
    }

    #[test]
    fn combined_transaction_never_changes_more_than_128_rows() {
        let mut db = fixture();
        // Shell event candidates precede the pending delivery candidates.
        for n in 0..32 {
            event(&db, &format!("shell-{n}"), "shell_ended", "done", "now");
        }
        for n in 0..64 {
            let id = format!("context-{n}");
            event(&db, &id, "context_changed", "done", "now");
            delivery(&db, &id, "done", "pending");
        }
        for n in 0..32 {
            delivery(&db, &format!("shell-{n}"), "done", "processed");
        }
        // Count the retained tables' rows: total_changes() also counts the
        // schema22 usage-counter trigger updates (#171).
        let rows = |db: &Connection| {
            db.query_row(
                "SELECT (SELECT count(*) FROM delivery)+(SELECT count(*) FROM event)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        let before = rows(&db);
        collect_batch(&mut db, Cursor::default()).unwrap();
        assert_eq!(before - rows(&db), 128);
    }

    #[tokio::test]
    async fn version_20_upgrade_adds_indexes_without_rewriting_rows() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
            db.execute_batch("DROP INDEX node_event_retention; DROP INDEX delivery_event_state; DROP INDEX activity_attempt_turn; DROP INDEX activity_turn_run; DROP INDEX activity_turn_run_state; DROP INDEX activity_group_turn_state; DROP INDEX activity_shell_spawned_turn; DROP INDEX activity_shell_uncollected_owner; DROP INDEX IF EXISTS receipt_reply_pending; DROP INDEX IF EXISTS history_tool_group_attempt; DROP INDEX IF EXISTS jobs_running_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN archived_at; DELETE FROM schema_migration WHERE version>=21; PRAGMA user_version=20; INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','stopped','now'); INSERT INTO event(id,kind,source_json,target_thread_id,target_run_id,payload_ref_json,created_at) VALUES('e','shell_ended','{}','t','r','{}','now'); INSERT INTO delivery(event_id,recipient_id,state) VALUES('e','r','pending');").unwrap();
        }
        for _ in 0..2 {
            let db = crate::storage::DbWorker::open(&path).await.unwrap();
            db.call(|db| {
                assert_eq!(db.query_row("PRAGMA user_version", [], |r|r.get::<_,i64>(0))?,crate::storage::SCHEMA_VERSION);
                assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('node_event_retention','delivery_event_state','jobs_source','nodes_index_gc')", [], |r|r.get::<_,i64>(0))?,4);
                assert_eq!(db.query_row("SELECT name FROM schema_migration WHERE version=20", [], |r|r.get::<_,String>(0))?,"0020_gc_index_source");
                assert_eq!(db.query_row("SELECT name FROM schema_migration WHERE version=21", [], |r|r.get::<_,String>(0))?,"0021_event_retention");
                assert_eq!(db.query_row("SELECT name FROM schema_migration WHERE version=22", [], |r|r.get::<_,String>(0))?,"0022_logical_usage_counters");
                assert_eq!(db.query_row("SELECT name FROM schema_migration WHERE version=23", [], |r|r.get::<_,String>(0))?,"0023_activity_bounded");
                assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('activity_attempt_turn','activity_turn_run','activity_turn_run_state','activity_group_turn_state','activity_shell_spawned_turn','activity_shell_uncollected_owner')", [], |r|r.get::<_,i64>(0))?,6);
                assert_eq!(db.query_row("SELECT count(*) FROM delivery WHERE event_id='e' AND state='pending'", [], |r|r.get::<_,i64>(0))?,1);
                Ok(())
            }).await.unwrap();
            db.shutdown().await.unwrap();
        }
    }

    #[test]
    fn reverse_node_lookup_uses_bounded_index() {
        let db = fixture();
        let mut stmt = db
            .prepare(&format!("EXPLAIN QUERY PLAN {ELIGIBLE_SQL}"))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map([1], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|line| line.contains("SEARCH n USING INDEX node_event_retention")),
            "{plan:?}"
        );
        assert_eq!(
            plan.iter()
                .filter(
                    |line| line.contains("SEARCH d USING COVERING INDEX delivery_event_state")
                        && (line.contains("state>") || line.contains("state<"))
                )
                .count(),
            3,
            "{plan:?}"
        );
    }
}
