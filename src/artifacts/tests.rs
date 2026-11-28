use super::*;

#[test]
fn artifact_list_seeks_across_equal_update_times() {
    let mut db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute_batch(
        "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)
         VALUES ('t','/','','p','m','now'), ('other','/','','p','m','now');
         INSERT INTO artifact(id,thread_id,created_at,updated_at) VALUES
         ('z','t','2026-11-01','2026-11-04'), ('a','t','2026-11-03','2026-11-03'),
         ('b','t','2026-11-03','2026-11-03'), ('c','t','2026-11-03','2026-11-03'),
         ('d','t','2026-11-02','2026-11-02'), ('other','other','2026-11-05','2026-11-05');
         INSERT INTO artifact_version(artifact_id,version,uri,title,created_at)
         SELECT id,1,'opaque:path','Output',created_at FROM artifact;",
    )
    .unwrap();
    let mut args = ArtifactListArgs {
        thread_id: Some("t".into()),
        scheme: None,
        limit: 2,
        cursor: None,
    };
    for expected in [["z", "a"], ["b", "c"]] {
        let result = list(&tx, &args).unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|a| a.artifact_id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        args.cursor = Some(result.next_cursor.unwrap());
    }
    let mut other = args.clone();
    other.thread_id = Some("other".into());
    assert_eq!(
        list(&tx, &other).unwrap_err().code,
        ErrorCode::CursorExpired
    );
    let result = list(&tx, &args).unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].artifact_id, "d");
    assert_eq!(result.items[0].owner_title, None);
    assert!(result.next_cursor.is_none());
    let old = page(
        vec!["a", "b"],
        1,
        "artifact.list",
        "t",
        ArtifactListFilter {
            thread_id: Some("t".into()),
            scheme: None,
        },
        |id| (*id).into(),
    )
    .unwrap();
    args.cursor = old.next_cursor;
    assert_eq!(list(&tx, &args).unwrap_err().code, ErrorCode::CursorExpired);
}

#[test]
fn artifact_scheme_pages_resume_after_bounded_filtered_candidates() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute_batch(
        "INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at,archived_at)
         VALUES ('live','/','','p','m','now',NULL), ('archive','/','','p','m','now','now');
         UPDATE thread SET title='Live owner' WHERE id='live';
         UPDATE thread SET title='Archived owner' WHERE id='archive';",
    )
    .unwrap();
    let add = |thread: &str, uri: &str, updated: &str| {
        super::add(
            &tx,
            &ArtifactAddArgs {
                thread_id: thread.into(),
                uri: uri.into(),
                title: "Fixture".into(),
                mime: None,
            },
            updated,
        )
        .unwrap()
        .artifact_id
    };
    let expected = add("live", "shellboy-app:/live", "2026-11-01");
    for _ in 0..SCHEME_CANDIDATE_LIMIT + 1 {
        add("archive", "shellboy-app:/archived", "2026-11-03");
    }
    let mut args = ArtifactListArgs {
        thread_id: None,
        scheme: Some("shellboy-app".into()),
        limit: 2,
        cursor: None,
    };
    let empty = list(&tx, &args).unwrap();
    assert!(empty.items.is_empty());
    args.cursor = Some(empty.next_cursor.unwrap());
    let continued = list(&tx, &args).unwrap();
    assert_eq!(continued.items[0].artifact_id, expected);
    assert_eq!(
        continued.items[0].owner_title.as_deref(),
        Some("Live owner")
    );
    assert!(continued.next_cursor.is_none());

    // Outside the Thread range, archived app rows must not consume the scoped budget.
    args.thread_id = Some("live".into());
    args.cursor = None;
    let scoped = list(&tx, &args).unwrap();
    assert_eq!(scoped.items[0].artifact_id, expected);
    assert_eq!(scoped.items[0].owner_title.as_deref(), Some("Live owner"));
    for _ in 0..SCHEME_CANDIDATE_LIMIT {
        add("live", "file:///other", "2026-11-02");
    }
    let empty = list(&tx, &args).unwrap();
    assert!(empty.items.is_empty());
    args.cursor = Some(empty.next_cursor.unwrap());
    let continued = list(&tx, &args).unwrap();
    assert_eq!(continued.items[0].artifact_id, expected);
    assert!(continued.next_cursor.is_none());
}

#[test]
fn artifact_scheme_page_uses_one_snapshot_across_owner_deletion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.db");
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer
        .execute_batch(crate::storage::CURRENT_SCHEMA)
        .unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL;
        INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at,archived_at) VALUES
        ('a','Owner A','/','','p','m','now',NULL),('b','Owner B','/','','p','m','now',NULL),
        ('c','','/','','p','m','now',NULL),('archived','Archived owner','/','','p','m','now','now');
        INSERT INTO artifact(id,thread_id,created_at,updated_at,current_scheme) VALUES
        ('a-app','a','1','4','shellboy-app'),('b-app','b','1','3','shellboy-app'),
        ('c-app','c','1','2','shellboy-app'),('archived-app','archived','1','5','shellboy-app');
        INSERT INTO artifact_version(artifact_id,version,uri,title,created_at)
        SELECT id,1,'shellboy-app:/fixture','App','1' FROM artifact;").unwrap();
    let mut reader = rusqlite::Connection::open(&path).unwrap();
    let snapshot = reader.transaction().unwrap();
    let first = list(
        &snapshot,
        &ArtifactListArgs {
            thread_id: None,
            scheme: Some("shellboy-app".into()),
            limit: 1,
            cursor: None,
        },
    )
    .unwrap();
    assert_eq!(first.items[0].owner_title.as_deref(), Some("Owner A"));
    writer.execute_batch("BEGIN; DELETE FROM artifact WHERE id='b-app'; UPDATE thread SET deleted_at='now' WHERE id='b'; UPDATE thread SET title='Renamed' WHERE id='c'; COMMIT;").unwrap();
    let args = ArtifactListArgs {
        thread_id: None,
        scheme: Some("shellboy-app".into()),
        limit: 4,
        cursor: first.next_cursor,
    };
    let pair = |page: RecordPage<ArtifactSummary, ArtifactListFilter>| {
        page.items
            .into_iter()
            .map(|row| (row.artifact_id, row.owner_title.unwrap()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pair(list(&snapshot, &args).unwrap()),
        [
            ("b-app".into(), "Owner B".into()),
            ("c-app".into(), String::new())
        ]
    );
    snapshot.commit().unwrap();
    let fresh = reader.transaction().unwrap();
    assert_eq!(
        pair(list(&fresh, &args).unwrap()),
        [("c-app".into(), "Renamed".into())]
    );
}
