use super::*;
use migration_integrity::{Check, pending, persist};

fn orphan(db: &Connection) {
    db.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO run(id,thread_id,state,created_at) VALUES('orphan','missing','completed','now');").unwrap();
}

#[test]
fn run_rebuild_upgrade_requires_full_scan_and_preserves_pending_on_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let mut db = Connection::open(&path).unwrap();
    build_historical_schema(&mut db, 26);
    orphan(&db);
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Full));
    assert!(Check::Full.run(&db).is_err());
}

#[test]
fn real_upgrade_without_metadata_checks_full_and_keeps_pending_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let mut db = Connection::open(&path).unwrap();
    build_historical_schema(&mut db, 23); // Migration24 has no impact metadata.
    orphan(&db);
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Full));
    // Migration 39 now rejects the orphan before commit, leaving version 38.
    assert_eq!(schema_version(&db).unwrap(), 38);
    assert!(Check::Full.run(&db).is_err());
    assert_eq!(
        db.query_row("SELECT thread_id FROM run WHERE id='orphan'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "missing"
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM schema_migration WHERE version=39",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Full));
    assert_eq!(schema_version(&db).unwrap(), 38);
    db.execute("DELETE FROM run WHERE id='orphan'", []).unwrap();
    drop(db);
    let db = open_connection(&path, None, None, true).unwrap();
    assert_eq!(schema_version(&db).unwrap(), SCHEMA_VERSION);
    assert_eq!(pending(&db).unwrap(), None);
}

#[test]
fn scoped_pending_survives_current_version_reopen_and_checks_only_its_tables() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let db = open_connection(&path, None, None, true).unwrap();
    db.execute_batch("CREATE TABLE fk_parent(id INTEGER PRIMARY KEY); CREATE TABLE fk_child(parent_id REFERENCES fk_parent(id));").unwrap();
    orphan(&db);
    persist(&db, Check::Tables(["fk_child".into()].into())).unwrap();
    drop(db);
    let db = open_connection(&path, None, None, true).unwrap();
    assert_eq!(pending(&db).unwrap(), None);
    db.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO fk_child VALUES(123);")
        .unwrap();
    persist(&db, Check::Tables(["fk_child".into()].into())).unwrap();
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert!(matches!(pending(&db).unwrap(), Some(Check::Tables(_))));
}

#[test]
fn interrupted_upgrade_and_later_integrity_error_both_keep_pending() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let mut db = Connection::open(&path).unwrap();
    build_historical_schema(&mut db, 26);
    db.execute_batch("CREATE TABLE jobs_running_recovery(id INTEGER);")
        .unwrap();
    drop(db);
    assert!(open_connection(&path, None, None, true).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Full));
    assert_eq!(schema_version(&db).unwrap(), 26);
    db.execute_batch("DROP TABLE jobs_running_recovery;")
        .unwrap();
    drop(db);
    let db = open_connection(&path, None, None, true).unwrap();
    assert_eq!(pending(&db).unwrap(), None);
    persist(&db, Check::Skip).unwrap();
    db.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('corrupt','fixture','{}','hash','failed','malformed','now');").unwrap();
    drop(db);
    assert!(open_connection(&path, None, None, false).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(pending(&db).unwrap(), Some(Check::Skip));
}

#[test]
fn artifact_scheme_migration_backfills_latest_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut db = Connection::open(&path).unwrap();
    let version = MIGRATION_NAMES
        .iter()
        .find(|(_, name)| *name == "0069_instruction_snapshot_turns")
        .unwrap()
        .0;
    build_historical_schema(&mut db, version as usize);
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');
        INSERT INTO artifact(id,thread_id,created_at,updated_at) VALUES ('a','t','now','now'),('b','t','now','now'),('c','t','now','now');
        INSERT INTO artifact_version(artifact_id,version,uri,title,created_at) VALUES
        ('a',1,'shellboy-app:/old','Old','now'),('a',2,'FiLe:///latest','Latest','now'),
        ('b',1,'/path:with-colon','Path','now'),('c',1,'SHELLBOY-APP:/app','App','now');").unwrap();
    drop(db);
    let db = open_connection(&path, None, None, true).unwrap();
    for (id, expected) in [
        ("a", Some("file")),
        ("b", None),
        ("c", Some("shellboy-app")),
    ] {
        let scheme: Option<String> = db
            .query_row(
                "SELECT current_scheme FROM artifact WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scheme.as_deref(), expected);
    }
    assert_eq!(schema_version(&db).unwrap(), SCHEMA_VERSION);
}
