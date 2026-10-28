use rusqlite::{Connection, params};
use shellboy::{
    history::{History, provenance, relations},
    protocol::{ErrorCode, HistoryGetArgs, HistoryRelation, HistoryRelationsArgs},
    storage::DbWorker,
};

fn request(
    node: &str,
    relation: HistoryRelation,
    limit: u32,
    cursor: Option<String>,
) -> HistoryRelationsArgs {
    HistoryRelationsArgs {
        node_id: node.into(),
        relation,
        limit,
        cursor,
    }
}

fn node(db: &Connection, id: &str, kind: &str) {
    db.execute("INSERT INTO node(id,kind,created_at,payload_json) VALUES(?1,?2,'2026-10-25T00:00:00Z','{\"text\":\"fixture\"}')", params![id,kind]).unwrap();
}

async fn fixture() -> (tempfile::TempDir, DbWorker) {
    let dir = tempfile::tempdir().unwrap();
    let db = DbWorker::open(dir.path().join("relations.db"))
        .await
        .unwrap();
    (dir, db)
}

#[tokio::test]
async fn direct_edges_keep_shared_diamond_separate_from_replacement_sources_and_are_readonly() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        for id in ["a","b","c","d","summary"] { node(db,id,"user"); }
        db.execute_batch("INSERT INTO edge VALUES('a','b'),('a','c'),('b','d'),('c','d');
            INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES('rep','summary','fixture','1','1','completed','now');
            INSERT INTO replacement_source VALUES('rep',0,'b',0,'original-b'),('rep',1,'a',0,'original-a');")?;
        let before=db.total_changes();
        let previous=relations(db,&request("d",HistoryRelation::Previous,50,None)).unwrap();
        assert_eq!(previous.items.iter().map(|i|i.node_id.as_str()).collect::<Vec<_>>(),vec!["b","c"]);
        let next=relations(db,&request("a",HistoryRelation::Next,50,None)).unwrap();
        assert_eq!(next.items.iter().map(|i|i.node_id.as_str()).collect::<Vec<_>>(),vec!["b","c"]);
        assert!(relations(db,&request("summary",HistoryRelation::Previous,50,None)).unwrap().items.is_empty());
        let sources=relations(db,&request("summary",HistoryRelation::Sources,50,None)).unwrap();
        assert_eq!(sources.items.iter().map(|i|i.node_id.as_str()).collect::<Vec<_>>(),vec!["b","a"]);
        assert_eq!(sources.items[0].source_hash.as_deref(),Some("original-b"));
        let replacements=relations(db,&request("a",HistoryRelation::Replacements,50,None)).unwrap();
        assert_eq!(replacements.items[0].node_id,"summary");
        assert_eq!(replacements.items[0].replacement_id.as_deref(),Some("rep"));
        assert_eq!(db.total_changes(),before);
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn provenance_records_actual_origin_without_inventing_turns_or_thread_membership() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('origin','/fixture','/fixture/AGENTS.md','proxy','fixture','now'),('fork','/fixture','/fixture/AGENTS.md','proxy','fixture','now');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('run','origin','completed','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('turn','origin','run','completed','now');")?;
        node(db,"owned","assistant_response");
        node(db,"unassigned","user");
        db.execute("UPDATE node SET turn_id='turn' WHERE id='owned'",[])?;
        db.execute_batch("INSERT INTO thread_path VALUES('origin',0,'owned'),('fork',0,'owned');")?;
        let actual=provenance(db,"owned").unwrap();
        assert_eq!(actual.origin_thread_id.as_deref(),Some("origin"));
        assert_eq!(actual.run_id.as_deref(),Some("run"));
        assert_eq!(actual.turn_id.as_deref(),Some("turn"));
        let unassigned=provenance(db,"unassigned").unwrap();
        assert!(unassigned.turn_id.is_none() && unassigned.run_id.is_none() && unassigned.origin_thread_id.is_none());
        assert_eq!(provenance(db,"missing").unwrap_err().code,ErrorCode::TargetUnavailable);
        Ok(())
    }).await.unwrap();
    let body = History::new(db.clone())
        .get(HistoryGetArgs {
            node_id: "owned".into(),
            limit_bytes: 65536,
            cursor: None,
            encoding: "UTF-8".into(),
        })
        .await
        .unwrap();
    assert_eq!(body.provenance.origin_thread_id.as_deref(), Some("origin"));
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn summary_and_spill_source_pages_preserve_ordinals_and_recorded_witness_after_tombstone() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        for (out,kind) in [("summary","summary"),("spill","spill")] {
            node(db,out,kind);
            db.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES(?1,?1,?1,'1','1','completed','now')",[out])?;
        }
        for ordinal in (0..241).rev() {
            let id=format!("source-{ordinal:03}");
            node(db,&id,"user");
            for out in ["summary","spill"] {
                db.execute("INSERT INTO replacement_source VALUES(?1,?2,?3,0,'recorded-hash')",params![out,ordinal,id])?;
            }
        }
        db.execute("UPDATE node SET payload_json=NULL,payload_revision=1,deleted_reason='fixture collection' WHERE id='source-000'",[])?;
        for out in ["summary","spill"] {
            let first=relations(db,&request(out,HistoryRelation::Sources,200,None)).unwrap();
            assert_eq!(first.items.len(),200);
            assert_eq!(first.items[0].node_id,"source-000");
            assert_eq!(first.items[0].provenance.source_revision,1);
            assert_eq!(first.items[0].source_revision,Some(0));
            assert_eq!(first.items[0].source_hash.as_deref(),Some("recorded-hash"));
            let second=relations(db,&request(out,HistoryRelation::Sources,200,first.next_cursor)).unwrap();
            assert_eq!(second.items.len(),41);
            assert_eq!(second.items[0].node_id,"source-200");
            assert!(second.next_cursor.is_none());
        }
        Ok(())
    }).await.unwrap();
    let tombstone = History::new(db.clone())
        .get(HistoryGetArgs {
            node_id: "source-000".into(),
            limit_bytes: 65536,
            cursor: None,
            encoding: "UTF-8".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        tombstone.tombstone_reason.as_deref(),
        Some("fixture collection")
    );
    assert_eq!(tombstone.provenance.source_revision, 1);
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn cursors_bind_node_relation_revision_and_watermark_without_expiring_on_unrelated_append() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        for id in ["root","a","b","c","other"] { node(db,id,"user"); }
        db.execute_batch("INSERT INTO edge VALUES('root','a'),('root','b');")?;
        let first=relations(db,&request("root",HistoryRelation::Next,1,None)).unwrap();
        let cursor=first.next_cursor.unwrap();
        for (node,relation) in [("other",HistoryRelation::Next),("root",HistoryRelation::Previous)] {
            assert_eq!(relations(db,&request(node,relation,1,Some(cursor.clone()))).unwrap_err().code,ErrorCode::CursorExpired);
        }
        db.execute_batch("INSERT INTO edge VALUES('root','c'),('other','c');")?;
        let second=relations(db,&request("root",HistoryRelation::Next,200,Some(cursor.clone()))).unwrap();
        assert_eq!(second.items.len(),1);
        assert_eq!(second.items[0].node_id,"b");
        assert!(second.next_cursor.is_none());
        assert_eq!(relations(db,&request("root",HistoryRelation::Next,200,None)).unwrap().items.len(),3);
        db.execute("UPDATE node SET payload_json=NULL,payload_revision=1,deleted_reason='collected' WHERE id='root'",[])?;
        assert_eq!(relations(db,&request("root",HistoryRelation::Next,1,Some(cursor))).unwrap_err().code,ErrorCode::CursorExpired);
        assert_eq!(relations(db,&request("root",HistoryRelation::Next,0,None)).unwrap_err().code,ErrorCode::InvalidArgument);
        assert_eq!(relations(db,&request("root",HistoryRelation::Next,201,None)).unwrap_err().code,ErrorCode::InvalidArgument);
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn replacement_pages_only_expose_completed_records_and_bound_the_initial_witness_set() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        for id in ["summary", "unfinished", "a", "b", "c", "late"] {
            node(db, id, "user");
        }
        db.execute_batch("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES('rep','summary','fixture','1','1','completed','now'),('pending','unfinished','pending','1','1','pending','now');
            INSERT INTO replacement_source VALUES('rep',0,'a',0,NULL),('rep',1,'b',0,NULL),('rep',2,'c',0,NULL),('pending',0,'a',0,NULL);")?;
        let first = relations(db, &request("summary", HistoryRelation::Sources, 1, None)).unwrap();
        db.execute("INSERT INTO replacement_source VALUES('rep',3,'late',0,NULL)", [])?;
        let second = relations(db, &request("summary", HistoryRelation::Sources, 200, first.next_cursor)).unwrap();
        assert_eq!(second.items.iter().map(|item| item.node_id.as_str()).collect::<Vec<_>>(), vec!["b", "c"]);
        assert!(second.next_cursor.is_none());
        assert!(relations(db, &request("unfinished", HistoryRelation::Sources, 50, None)).unwrap().items.is_empty());
        let outputs = relations(db, &request("a", HistoryRelation::Replacements, 50, None)).unwrap();
        assert_eq!(outputs.items.len(), 1);
        assert_eq!(outputs.items[0].node_id, "summary");
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}
