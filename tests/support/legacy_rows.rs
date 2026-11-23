//! Move test-generated rows into a separately built historical database.
use std::path::Path;

use rusqlite::Connection;

pub(crate) fn seed(path: &Path, version: usize, tables: &[&str]) {
    seed_with(path, version, tables, |_| {});
}

pub(crate) fn seed_with(
    path: &Path,
    version: usize,
    tables: &[&str],
    extra: impl FnOnce(&rusqlite::Transaction<'_>),
) {
    let staged = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
    {
        let mut db = Connection::open(staged.path()).unwrap();
        crate::schema42_fixture::build_historical_schema(&mut db, version);
        db.execute("ATTACH DATABASE ?1 AS generated", [path.to_str().unwrap()])
            .unwrap();
        let tx = db.transaction().unwrap();
        tx.pragma_update(None, "defer_foreign_keys", true).unwrap();
        for table in tables {
            // Copy only fields that existed in the pinned schema. The table list is
            // local to each witness; neither schema metadata nor new tables move.
            let columns: String = tx.query_row(
                "SELECT group_concat(printf('\"%w\"', old.name)) FROM pragma_table_info(?1,'main') old
                 JOIN pragma_table_info(?1,'generated') new USING(name)",
                [table],
                |row| row.get(0),
            ).unwrap();
            tx.execute_batch(&format!(
                "INSERT INTO main.{table}({columns}) SELECT {columns} FROM generated.{table}"
            ))
            .unwrap_or_else(|e| panic!("seed {table} at schema {version}: {e}"));
        }
        extra(&tx);
        let violation = tx
            .prepare("PRAGMA main.foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            !violation,
            "legacy rows must satisfy schema {version} foreign keys"
        );
        tx.commit().unwrap();
    }
    staged.persist(path).unwrap();
}
