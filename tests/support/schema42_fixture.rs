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

/// Rewind a database freshly created by current production code (schema 43)
/// to the schema-42 shape, so hand-built older fixtures can exercise upgrades.
pub(crate) const REWIND_43_TO_42: &str = "DROP TABLE summary_usage_stage_archive;
 DROP TABLE thread_retired_summary_settings;
 ALTER TABLE thread DROP COLUMN summary_max_chars;
 ALTER TABLE thread DROP COLUMN auto_compact_target_percent;
 ALTER TABLE thread ADD COLUMN segment_chars INTEGER CHECK(segment_chars IS NULL OR segment_chars BETWEEN 2000 AND 1000000);
 ALTER TABLE thread ADD COLUMN reduce_rounds INTEGER CHECK(reduce_rounds IS NULL OR reduce_rounds BETWEEN 1 AND 16);
 DELETE FROM schema_migration WHERE version=43;
 PRAGMA user_version=42;";
