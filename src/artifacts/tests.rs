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
         VALUES ('live','/','','p','m','now',NULL), ('archive','/','','p','m','now','now');",
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
    assert!(continued.next_cursor.is_none());

    // Outside the Thread range, archived app rows must not consume the scoped budget.
    args.thread_id = Some("live".into());
    args.cursor = None;
    assert_eq!(list(&tx, &args).unwrap().items[0].artifact_id, expected);
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
