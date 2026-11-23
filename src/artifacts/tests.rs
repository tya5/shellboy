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
        thread_id: "t".into(),
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
    other.thread_id = "other".into();
    assert_eq!(
        list(&tx, &other).unwrap_err().code,
        ErrorCode::CursorExpired
    );
    let result = list(&tx, &args).unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].artifact_id, "d");
    assert!(result.next_cursor.is_none());
    let old = page(vec!["a", "b"], 1, "artifact.list", "t", |id| (*id).into()).unwrap();
    args.cursor = old.next_cursor;
    assert_eq!(list(&tx, &args).unwrap_err().code, ErrorCode::CursorExpired);
}
