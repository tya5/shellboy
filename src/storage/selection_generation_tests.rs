use super::*;
use rusqlite::OptionalExtension;

fn fresh() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(CURRENT_SCHEMA).unwrap();
    db
}

fn column_exists(db: &Connection) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('thread') WHERE name='selection_nonappend_generation')",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn v46_database() -> Connection {
    let db = fresh();
    db.execute_batch(
        "DELETE FROM schema_migration WHERE version=47;
         DELETE FROM schema_migration WHERE version=40;
         DROP TRIGGER selection_thread_conflict;
         PRAGMA user_version=39;",
    )
    .unwrap();
    db.execute_batch(include_str!(
        "../../migrations/0040_selection_nonappend_generation.sql"
    ))
    .unwrap();
    db.execute_batch("PRAGMA user_version=46;").unwrap();
    db
}

fn trigger_names(db: &Connection) -> Vec<String> {
    db.prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'selection_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn fresh_schema_keeps_migration_40_and_unrelated_thread_conflict_guard_only() {
    let db = fresh();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    assert!(!column_exists(&db));
    assert_eq!(
        trigger_names(&db),
        ["selection_thread_conflict"].map(str::to_owned)
    );
    assert_eq!(
        db.query_row(
            "SELECT name FROM schema_migration WHERE version=40",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "0040_selection_nonappend_generation"
    );
}

#[test]
fn migration_47_drops_runtime_generation_with_rollback_and_preserves_history_and_integrity() {
    let mut db = v46_database();
    assert!(column_exists(&db));
    db.execute_batch(
        "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,selection_nonappend_generation)
         VALUES('t','/','','p','m','now',23);
         INSERT INTO node(id,kind,created_at,payload_json) VALUES('n','user','now','{}');
         INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES('t',0,'n');",
    )
    .unwrap();
    {
        let tx = db.transaction().unwrap();
        tx.execute_batch(include_str!(
            "../../migrations/0047_drop_selection_nonappend_generation.sql"
        ))
        .unwrap();
        assert!(!column_exists(&tx));
    }
    assert!(
        column_exists(&db),
        "dropping the migration transaction rolls back"
    );
    super::migrate::run(&mut db, 46, &|_, _| {}, &|| {}).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        47
    );
    assert!(!column_exists(&db));
    assert_eq!(
        trigger_names(&db),
        ["selection_thread_conflict"].map(str::to_owned)
    );
    assert_eq!(
        db.query_row(
            "SELECT name FROM schema_migration WHERE version=40",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "0040_selection_nonappend_generation"
    );
    assert_eq!(
        db.query_row(
            "SELECT name FROM schema_migration WHERE version=47",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "0047_drop_selection_nonappend_generation"
    );
    assert_eq!(
        db.query_row(
            "SELECT node_id FROM thread_path WHERE thread_id='t'",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "n"
    );
    assert_eq!(
        db.query_row("PRAGMA foreign_key_check", [], |row| row
            .get::<_, Option<String>>(0))
            .optional()
            .unwrap(),
        None
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
