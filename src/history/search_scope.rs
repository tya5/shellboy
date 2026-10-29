//! Shared Thread search membership: selected path plus completed replacement sources.
use rusqlite::{Connection, params};

/// Parameter is a fixed SQL placeholder supplied by our query, never user input.
pub(crate) fn cte(parameter: &str) -> String {
    format!(
        "WITH RECURSIVE scope(id) AS (SELECT node_id FROM thread_path WHERE thread_id={parameter} UNION SELECT rs.source_node_id FROM scope s JOIN replacement r ON r.output_node_id=s.id AND r.state='completed' JOIN replacement_source rs ON rs.replacement_id=r.id)"
    )
}

pub(crate) fn exists(db: &Connection, thread: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
        [thread],
        |row| row.get(0),
    )
}

pub(crate) fn on_current_path(db: &Connection, thread: &str, node: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2)",
        params![thread, node],
        |row| row.get(0),
    )
}

/// Recheck membership inside a hit's current read snapshot.
pub(crate) fn contains(db: &Connection, thread: &str, node: &str) -> rusqlite::Result<bool> {
    let sql = format!(
        "{} SELECT EXISTS(SELECT 1 FROM scope WHERE id=?2)",
        cte("?1")
    );
    db.query_row(&sql, params![thread, node], |row| row.get(0))
}
