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
