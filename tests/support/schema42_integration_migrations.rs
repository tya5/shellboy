//! Downgrade helpers used only by the integration-test migration fixtures.

use rusqlite::Connection;

/// Reverse migrations 40–42 from the pinned schema42 baseline while preserving
/// the schema39 run ownership/event model for the integration fixture's own
/// 39→38 downgrade and assertions.
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
    .expect("rewind pinned schema42 integration fixture to schema39");
}
