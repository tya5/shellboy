//! Shared pinned schema-42 baseline for historical Rust migration fixtures.

use rusqlite::Connection;
use std::path::Path;

/// Open the exact pinned schema-42 database used by historical upgrade tests.
pub(crate) fn open_schema42_fixture(path: &Path) -> Connection {
    let db = Connection::open(path).expect("open schema-42 fixture database");
    db.execute_batch(include_str!("../fixtures/storage-current-schema42.sql"))
        .expect("load pinned schema-42 fixture");
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        42,
        "pinned historical fixture must end at schema 42"
    );
    db
}

/// Reverse schema42-only migrations 40–42 while retaining the schema39 run
/// ownership/event model. A caller can then apply its original 39→38 step.
pub(crate) fn downgrade_schema42_to_schema39(db: &Connection) {
    db.execute_batch(
        "DROP TRIGGER selection_generation_combined_write;
         DROP TRIGGER selection_generation_update;
         DROP TRIGGER selection_thread_conflict;
         DROP TRIGGER selection_generation_insert;
         DROP TRIGGER selection_path_insert;
         DROP TRIGGER selection_path_delete;
         DROP TRIGGER selection_path_update;
         DROP TRIGGER selection_revision_same_cursor;
         ALTER TABLE thread DROP COLUMN selection_nonappend_generation;
         DROP INDEX receipt_reply_node_lookup;
         DROP INDEX node_notification_event;
         ALTER TABLE node DROP COLUMN notification_event_id;
         ALTER TABLE event DROP COLUMN semantic_json;
         DELETE FROM schema_migration WHERE version>=40;
         PRAGMA user_version=39;",
    )
    .expect("rewind pinned schema42 fixture to schema39");
}
