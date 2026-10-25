//! Explicit liveness for scalar selected-path revision reads.
use rusqlite::Connection;
#[derive(Clone, Copy)]
pub(crate) enum ThreadScope {
    Live,
    Any,
}
pub(crate) fn selection_revision(
    db: &Connection,
    id: &str,
    scope: ThreadScope,
) -> rusqlite::Result<i64> {
    let sql = match scope {
        ThreadScope::Live => {
            "SELECT selection_revision FROM thread WHERE id=?1 AND deleted_at IS NULL"
        }
        ThreadScope::Any => "SELECT selection_revision FROM thread WHERE id=?1",
    };
    db.query_row(sql, [id], |row| row.get(0))
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;
    #[test]
    fn live_and_any_keep_deleted_and_missing_rows_distinct() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE thread(id TEXT,selection_revision INTEGER,deleted_at TEXT); INSERT INTO thread VALUES('live',7,NULL),('deleted',19,'now');").unwrap();
        assert_eq!(
            selection_revision(&db, "live", ThreadScope::Live).unwrap(),
            7
        );
        assert_eq!(
            selection_revision(&db, "live", ThreadScope::Any).unwrap(),
            7
        );
        assert_eq!(
            selection_revision(&db, "deleted", ThreadScope::Any).unwrap(),
            19
        );
        assert!(matches!(
            selection_revision(&db, "deleted", ThreadScope::Live),
            Err(rusqlite::Error::QueryReturnedNoRows)
        ));
        assert!(
            selection_revision(&db, "deleted", ThreadScope::Live)
                .optional()
                .unwrap()
                .is_none()
        );
        assert!(
            selection_revision(&db, "missing", ThreadScope::Any)
                .optional()
                .unwrap()
                .is_none()
        );
    }
}
