use super::*;

#[test]
fn export_cursor_keeps_saved_order_after_adoption_and_collection() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');
        INSERT INTO node(id,kind,created_at,payload_json) VALUES('a','user','now','{}'),('b','user','now','{}');
        INSERT INTO thread_path VALUES('t',0,'a'),('t',1,'b');").unwrap();
    let mut args = HistoryExportArgs {
        thread_id: "t".into(),
        cursor: None,
        limit: 1,
    };
    let first = read(&db, &args, "owner", "operation").unwrap();
    assert!(
        matches!(&first.items[..], [HistoryExportRecord::Node { node, role:HistoryExportRole::Current,.. }] if node.id=="a")
    );
    args.cursor = first.next_cursor;
    db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json) VALUES('summary','summary','now','{}');
        INSERT INTO replacement VALUES('r','summary','k','f','p','pruned','now');
        INSERT INTO replacement_source VALUES('r',0,'a',0,NULL),('r',1,'b',0,NULL);
        DELETE FROM thread_path WHERE thread_id='t'; INSERT INTO thread_path VALUES('t',0,'summary');
        UPDATE node SET payload_json=NULL, deleted_reason='pruned' WHERE id='b';").unwrap();
    let second = read(&db, &args, "owner", "operation").unwrap();
    assert!(
        matches!(&second.items[..], [HistoryExportRecord::Tombstone {node,role:HistoryExportRole::ReplacedSource,..}] if node.id=="b" && node.payload_json.is_none())
    );
    args.cursor = second.next_cursor;
    let third = read(&db, &args, "owner", "operation").unwrap();
    assert!(
        matches!(&third.items[..], [HistoryExportRecord::Node {node, ..}] if node.id=="summary")
    );
    assert!(third.next_cursor.is_none());
    assert_eq!(
        read(&db, &args, "other owner", "operation")
            .unwrap_err()
            .code,
        ErrorCode::CursorExpired
    );
}

#[test]
fn export_selected_tombstone_marks_external_and_missing_edges() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    // Model a legacy dangling edge to test defensive export classification.
    db.execute_batch("PRAGMA foreign_keys=OFF;
        INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','','p','m','now');
        INSERT INTO node(id,kind,created_at,deleted_reason) VALUES('bodyless','user','now','collected'),('external','user','now',NULL);
        INSERT INTO thread_path VALUES('t',0,'bodyless');
        INSERT INTO edge VALUES('external','bodyless'),('bodyless','missing');").unwrap();
    let args = HistoryExportArgs {
        thread_id: "t".into(),
        cursor: None,
        limit: 1,
    };
    let page = read(&db, &args, "owner", "operation").unwrap();
    let [
        HistoryExportRecord::Tombstone {
            role,
            node,
            relations,
            reason,
            ..
        },
    ] = &page.items[..]
    else {
        panic!("selected tombstone was lost")
    };
    assert_eq!(*role, HistoryExportRole::Current);
    assert_eq!(node.current_ordinal, Some(0));
    assert_eq!(reason, "collected");
    assert_eq!(
        relations.previous,
        [HistoryExportReference {
            id: "external".into(),
            scope: HistoryExportReferenceScope::External
        }]
    );
    assert_eq!(
        relations.next,
        [HistoryExportReference {
            id: "missing".into(),
            scope: HistoryExportReferenceScope::Missing
        }]
    );
}
