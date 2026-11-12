use super::*;
fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(CURRENT_SCHEMA).unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'),('other','/','','p','m','now'); INSERT INTO node(id,kind,created_at,payload_json) VALUES('a','user','now','{}'),('b','user','now','{}'),('c','user','now','{}');").unwrap();
    db
}
fn generation(db: &Connection, id: &str) -> i64 {
    db.query_row(
        "SELECT selection_nonappend_generation FROM thread WHERE id=?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}
#[test]
fn tails_do_not_bump_but_rewrites_upserts_and_owner_moves_do() {
    let db = db();
    db.execute_batch("INSERT INTO thread_path VALUES('t',0,'a'),('t',1,'b'); UPDATE thread SET cursor_node_id='b',selection_revision=selection_revision+1 WHERE id='t';").unwrap();
    assert_eq!(generation(&db, "t"), 0);
    db.execute_batch("INSERT INTO thread_path VALUES('t',1,'c') ON CONFLICT(thread_id,ordinal) DO UPDATE SET node_id=excluded.node_id;").unwrap();
    assert!(generation(&db, "t") > 0);
    let old = generation(&db, "t");
    db.execute_batch(
        "UPDATE thread_path SET thread_id='other',ordinal=0 WHERE thread_id='t' AND ordinal=1;",
    )
    .unwrap();
    assert_eq!(generation(&db, "t"), old + 1);
    assert_eq!(generation(&db, "other"), 1);
    let old = generation(&db, "t");
    db.execute_batch("UPDATE thread_path SET node_id=node_id WHERE thread_id='t';")
        .unwrap();
    assert_eq!(generation(&db, "t"), old + 1);
}
#[test]
fn replace_cannot_hide_delete_with_recursive_triggers_disabled() {
    let db = db();
    db.execute_batch("PRAGMA recursive_triggers=OFF; INSERT INTO thread_path VALUES('t',0,'a');")
        .unwrap();
    db.execute_batch("INSERT OR REPLACE INTO thread_path VALUES('t',0,'b');")
        .unwrap();
    assert!(generation(&db, "t") > 0);
    let old = generation(&db, "t");
    // UNIQUE(thread_id,node_id) replacement at what would otherwise be tail.
    db.execute_batch("INSERT OR REPLACE INTO thread_path VALUES('t',1,'b');")
        .unwrap();
    assert!(generation(&db, "t") > old);
    assert!(db.execute_batch("INSERT OR REPLACE INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');").is_err());
    assert!(db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now') ON CONFLICT(id) DO UPDATE SET selection_nonappend_generation=0;").is_err());
}
#[test]
fn rollback_metadata_and_overflow_fail_closed() {
    let mut db = db();
    db.execute_batch("INSERT INTO thread_path VALUES('t',0,'a');")
        .unwrap();
    {
        let tx = db.transaction().unwrap();
        tx.execute_batch("DELETE FROM thread_path WHERE thread_id='t';")
            .unwrap();
        assert!(generation(&tx, "t") > 0);
    }
    assert_eq!(generation(&db, "t"), 0);
    db.execute_batch("UPDATE thread SET selection_revision=selection_revision+1 WHERE id='t';")
        .unwrap();
    assert_eq!(generation(&db, "t"), 1);
    assert!(
        db.execute_batch("UPDATE thread SET selection_nonappend_generation=0 WHERE id='t';")
            .is_err()
    );
    assert!(
        db.execute_batch("UPDATE thread SET selection_nonappend_generation=1.5 WHERE id='t';")
            .is_err()
    );
    db.execute_batch(
        "UPDATE thread SET selection_nonappend_generation=9223372036854775807 WHERE id='t';",
    )
    .unwrap();
    assert!(
        db.execute_batch("DELETE FROM thread_path WHERE thread_id='t';")
            .is_err()
    );
    assert_eq!(generation(&db, "t"), i64::MAX);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM thread_path WHERE thread_id='t'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn migration_matches_fresh_schema_and_rolls_back_transactionally() {
    let fresh = db();
    let (prefix, _) = include_str!("../../tests/fixtures/storage-current-schema42.sql")
        .split_once("-- A revision writer must never")
        .unwrap();
    let previous=prefix.replace(", selection_nonappend_generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(selection_nonappend_generation)='integer' AND selection_nonappend_generation>=0)","");
    let mut old = Connection::open_in_memory().unwrap();
    old.execute_batch(&previous).unwrap();
    {
        let tx = old.transaction().unwrap();
        tx.execute_batch(include_str!(
            "../../migrations/0040_selection_nonappend_generation.sql"
        ))
        .unwrap();
    }
    assert_eq!(
        old.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        39
    );
    super::migrate::run(&mut old, 39, &|_, _| {}, &|| {}).unwrap();
    let definitions = |db: &Connection| {
        db.prepare("SELECT name,sql FROM sqlite_master WHERE type='trigger' AND name LIKE 'selection_%' ORDER BY name").unwrap().query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
    };
    assert_eq!(definitions(&fresh), definitions(&old));
    assert_eq!(
        old.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
}
#[test]
fn separate_connections_observe_committed_witness_only() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("synthetic.sqlite");
    let a = Connection::open(&path).unwrap();
    a.execute_batch(CURRENT_SCHEMA).unwrap();
    a.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now'); INSERT INTO node(id,kind,created_at,payload_json) VALUES('a','user','now','{}'),('b','user','now','{}'); INSERT INTO thread_path VALUES('t',0,'a');").unwrap();
    let mut b = Connection::open(&path).unwrap();
    {
        let tx = b.transaction().unwrap();
        tx.execute_batch("DELETE FROM thread_path WHERE thread_id='t';")
            .unwrap();
        assert_eq!(generation(&a, "t"), 0);
    }
    assert_eq!(generation(&a, "t"), 0);
    b.execute_batch("INSERT INTO thread_path VALUES('t',1,'b');")
        .unwrap();
    assert_eq!(generation(&a, "t"), 0);
    b.execute_batch("DELETE FROM thread_path WHERE thread_id='t' AND ordinal=1;")
        .unwrap();
    assert!(generation(&a, "t") > 0);
}

#[test]
fn stale_combined_revision_generation_writes_are_rejected_in_both_trigger_modes() {
    for recursive in ["OFF", "ON"] {
        let db = db();
        db.execute_batch(&format!("PRAGMA recursive_triggers={recursive};"))
            .unwrap();
        assert!(db.execute_batch("UPDATE thread SET selection_revision=selection_revision+1,selection_nonappend_generation=selection_nonappend_generation WHERE id='t';").is_err());
        assert_eq!(generation(&db, "t"), 0);
        db.execute_batch("UPDATE thread SET selection_revision=selection_revision+1 WHERE id='t';")
            .unwrap();
        assert_eq!(generation(&db, "t"), 1);
    }
}
