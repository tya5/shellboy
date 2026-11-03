//! Crash-persistent validation scopes for startup-only migrations.
use super::{StorageError, migrate::Migration};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const PENDING: &str = "integrity.migration_pending";

#[derive(Clone, Copy)]
pub(super) enum Impact {
    IndexOnly,
    Tables(&'static [&'static str]),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "tables", deny_unknown_fields)]
pub(super) enum Check {
    Skip,
    Tables(BTreeSet<String>),
    Full,
}

impl Check {
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Full, _) | (_, Self::Full) => Self::Full,
            (Self::Skip, other) | (other, Self::Skip) => other,
            (Self::Tables(mut a), Self::Tables(b)) => {
                a.extend(b);
                Self::Tables(a)
            }
        }
    }

    pub(super) fn for_migrations(migrations: &[Migration], from: i64, to: i64) -> Self {
        let mut scope = Self::Skip;
        for version in from + 1..=to {
            let effect = migrations
                .iter()
                .find(|entry| entry.version == version)
                .and_then(|entry| entry.foreign_keys);
            scope = scope.merge(match effect {
                Some(Impact::IndexOnly) => Self::Skip,
                Some(Impact::Tables(tables))
                    if !tables.is_empty() && tables.iter().all(|name| !name.is_empty()) =>
                {
                    Self::Tables(tables.iter().map(|name| (*name).to_owned()).collect())
                }
                _ => Self::Full,
            });
        }
        scope
    }

    pub(super) fn required(&self) -> bool {
        !matches!(self, Self::Skip)
    }

    pub(super) fn run(&self, db: &Connection) -> Result<(), StorageError> {
        match self {
            Self::Skip => Ok(()),
            Self::Full => super::check_integrity(db, true, false),
            Self::Tables(touched) => {
                // FK metadata names the child->parent edges. Include children of
                // changed parents, even when their own data was not migrated.
                let tables = db
                    .prepare("SELECT name FROM sqlite_schema WHERE type='table'")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut edges = Vec::new();
                for child in &tables {
                    let parents = db
                        .prepare("SELECT \"table\" FROM pragma_foreign_key_list(?1)")?
                        .query_map([child], |row| row.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?;
                    edges.extend(parents.into_iter().map(|parent| (child.clone(), parent)));
                }
                let mut selected = touched.clone();
                loop {
                    let before = selected.len();
                    for (child, parent) in &edges {
                        if selected
                            .iter()
                            .any(|name| name.eq_ignore_ascii_case(parent))
                        {
                            selected.insert(child.clone());
                        }
                    }
                    if selected.len() == before {
                        break;
                    }
                }
                for table in tables
                    .iter()
                    .filter(|table| selected.iter().any(|name| name.eq_ignore_ascii_case(table)))
                {
                    if db
                        .prepare("SELECT 1 FROM pragma_foreign_key_check(?1) LIMIT 1")?
                        .exists([table])?
                    {
                        return Err(StorageError::Schema("foreign key violations".into()));
                    }
                }
                Ok(())
            }
        }
    }
}

pub(super) fn pending(db: &Connection) -> Result<Option<Check>, StorageError> {
    let raw: Option<String> = db
        .query_row(
            "SELECT value FROM app_meta WHERE key=?1",
            [PENDING],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.map(|raw| match serde_json::from_str::<Check>(&raw) {
        Ok(Check::Tables(tables)) if tables.is_empty() || tables.iter().any(String::is_empty) => {
            Check::Full
        }
        Ok(scope) => scope,
        Err(_) => Check::Full,
    }))
}

pub(super) fn persist(db: &Connection, scope: Check) -> Result<(), StorageError> {
    let scope = pending(db)?.unwrap_or(Check::Skip).merge(scope);
    let json = serde_json::to_string(&scope)
        .map_err(|error| StorageError::InvalidInput(error.to_string()))?;
    db.execute(
        "INSERT OR REPLACE INTO app_meta(key,value) VALUES(?1,?2)",
        [PENDING, &json],
    )?;
    Ok(())
}

pub(super) fn clear(db: &Connection) -> Result<(), StorageError> {
    db.execute("DELETE FROM app_meta WHERE key=?1", [PENDING])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration(effect: Option<Impact>) -> Migration {
        Migration {
            version: 2,
            sql: "",
            foreign_keys: effect,
        }
    }

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE parent(id INTEGER PRIMARY KEY);
            CREATE TABLE child(parent_id INTEGER REFERENCES parent(id));
            CREATE TABLE unrelated(parent_id INTEGER REFERENCES parent(id));
            INSERT INTO unrelated VALUES(99);",
        )
        .unwrap();
        db
    }

    #[test]
    fn index_only_migration_skips_even_unrelated_corruption() {
        let db = fixture();
        let scope = Check::for_migrations(&[migration(Some(Impact::IndexOnly))], 1, 2);
        assert_eq!(scope, Check::Skip);
        scope.run(&db).unwrap();
        assert!(Check::Full.run(&db).is_err());
        assert_eq!(
            Check::for_migrations(super::super::migrate::MIGRATIONS, 24, 27),
            Check::Skip
        );
    }

    #[test]
    fn table_touch_checks_only_selected_tables_and_parent_dependencies() {
        let db = fixture();
        let scope = Check::for_migrations(&[migration(Some(Impact::Tables(&["child"])))], 1, 2);
        scope.run(&db).unwrap(); // The unrelated orphan is deliberately not scanned.
        db.execute("INSERT INTO child VALUES(88)", []).unwrap();
        assert!(scope.run(&db).is_err());
        db.execute("DELETE FROM child", []).unwrap();
        let parent = Check::for_migrations(&[migration(Some(Impact::Tables(&["PARENT"])))], 1, 2);
        assert!(
            parent.run(&db).is_err(),
            "a changed parent must check referencing children"
        );
    }

    #[test]
    fn missing_metadata_or_registry_entry_falls_back_to_full() {
        let db = fixture();
        for migrations in [
            vec![migration(None)],
            vec![],
            vec![migration(Some(Impact::Tables(&[])))],
        ] {
            let scope = Check::for_migrations(&migrations, 1, 2);
            assert_eq!(scope, Check::Full);
            assert!(scope.run(&db).is_err());
        }
        assert_eq!(
            Check::for_migrations(super::super::migrate::MIGRATIONS, 23, 27),
            Check::Full
        );
    }

    #[test]
    fn prior_pending_scope_cannot_be_downgraded_by_index_only_upgrade() {
        let db = fixture();
        persist(&db, Check::Tables(["child".into()].into())).unwrap();
        persist(&db, Check::Skip).unwrap();
        assert_eq!(
            pending(&db).unwrap(),
            Some(Check::Tables(["child".into()].into()))
        );
        persist(&db, Check::Tables(["unrelated".into()].into())).unwrap();
        assert!(pending(&db).unwrap().unwrap().run(&db).is_err());
        for raw in [
            "1",
            "{}",
            "{\"scope\":\"Tables\",\"tables\":[]}",
            "{\"scope\":\"future\"}",
        ] {
            db.execute("UPDATE app_meta SET value=?1", [raw]).unwrap();
            persist(&db, Check::Skip).unwrap();
            assert_eq!(pending(&db).unwrap(), Some(Check::Full));
        }
    }

    #[test]
    fn interrupted_check_stays_pending_across_reopen_at_current_version() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        let db = super::super::open_connection(&path, None, None, true).unwrap();
        persist(&db, Check::Full).unwrap();
        let scope = pending(&db).unwrap().unwrap();
        db.progress_handler(1, Some(|| true)).unwrap();
        assert!(scope.run(&db).is_err());
        db.progress_handler(0, None::<fn() -> bool>).unwrap();
        assert_eq!(pending(&db).unwrap(), Some(Check::Full));
        drop(db);
        // Simulate a restart after the migration already committed its version.
        let db = super::super::open_connection(&path, None, None, true).unwrap();
        assert_eq!(pending(&db).unwrap(), None);
    }

    #[test]
    fn registry_is_complete_and_uses_existing_migration_versions() {
        let versions: Vec<_> = super::super::migrate::MIGRATIONS
            .iter()
            .map(|entry| entry.version)
            .collect();
        assert_eq!(
            versions,
            (2..=super::super::SCHEMA_VERSION).collect::<Vec<_>>()
        );
    }
}
