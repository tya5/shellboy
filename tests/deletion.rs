use rusqlite::{Connection, params};
use shellboy::{
    attachments::AttachmentService,
    config::Config,
    deletion::preview,
    gc::GcService,
    protocol::{ErrorCode, ThreadDeleteArgs},
    storage::DbWorker,
};
use std::sync::Arc;

fn request(descendants: bool) -> ThreadDeleteArgs {
    ThreadDeleteArgs {
        thread_id: "parent".into(),
        descendants,
    }
}
fn thread(db: &Connection, id: &str, parent: Option<&str>) {
    db.execute("INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?2,'/fixture','/fixture/AGENTS.md','proxy','fixture','2026-10-26T00:00:00Z')",params![id,parent]).unwrap();
}
fn node(db: &Connection, id: &str) {
    db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes) VALUES(?1,'user','now','{\"text\":\"fixture\"}',18)",[id]).unwrap();
}
async fn fixture() -> (tempfile::TempDir, DbWorker) {
    let dir = tempfile::tempdir().unwrap();
    let db = DbWorker::open(dir.path().join("deletion.db"))
        .await
        .unwrap();
    (dir, db)
}

#[tokio::test]
async fn counts_distinct_shared_nodes_and_direct_sources_without_predicting_gc_or_touching_files() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        thread(db,"parent",None); thread(db,"child",Some("parent")); thread(db,"fork",None);
        for id in ["shared","private","summary"] { node(db,id); }
        db.execute_batch("INSERT INTO thread_path VALUES('parent',0,'shared'),('parent',1,'private'),('child',0,'shared'),('fork',0,'shared'),('fork',1,'summary');
            INSERT INTO replacement VALUES('rep','summary','key','1','1','completed','now');
            INSERT INTO replacement_source VALUES('rep',0,'private',0,'hash');
            INSERT INTO thread_replacement VALUES('fork','rep');
            INSERT INTO attachment_ref VALUES('a','shared','/nonexistent/same','file','external','now'),('b','shared','/nonexistent/same','file','external','now'),('c','private','/nonexistent/managed','image','managed_clipboard','now');")?;
        let before = db.total_changes();
        let p=preview(db,&request(true)).unwrap();
        assert_eq!(p["selection"]["thread_count"],2);
        assert_eq!(p["selection"]["descendant_thread_count"],1);
        assert_eq!(p["selection"]["surviving_descendant_thread_count"],0);
        assert_eq!(p["references"]["selected_path_nodes"],2);
        assert_eq!(p["references"]["shared_path_nodes"],1);
        assert_eq!(p["references"]["surviving_path_thread_count"],1);
        assert_eq!(p["references"]["completed_replacement_source_nodes"],1);
        assert_eq!(p["references"]["surviving_replacement_source_nodes"],1);
        assert_eq!(p["references"]["attachment_reference_count"],3);
        assert_eq!(p["references"]["external_attachment_reference_count"],2);
        assert_eq!(p["references"]["managed_attachment_reference_count"],1);
        assert_eq!(p["reference_scope"],"direct_references_not_gc_reachability");
        assert_eq!(p["deletable_now"],true);
        assert!(p["reclaimable_bytes"].is_null());
        assert_eq!(db.total_changes(),before);
        // Existing caller transaction is also supported without nested BEGIN.
        let tx=db.transaction()?;
        let one=preview(&tx,&request(false)).unwrap();
        assert_eq!(one["selection"]["thread_count"],1);
        assert_eq!(one["selection"]["surviving_descendant_thread_count"],1);
        assert_eq!(one["references"]["surviving_path_thread_count"],2);
        tx.rollback()?;
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn active_and_stopping_runs_block_only_selected_threads_with_explicit_detail_limit() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        thread(db, "parent", None);
        for i in 0..55 {
            let id = format!("child-{i:02}");
            thread(db, &id, Some("parent"));
            db.execute(
                "INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?1,?2,'now')",
                params![id, if i % 2 == 0 { "live" } else { "stopping" }],
            )?;
        }
        let one = preview(db, &request(false)).unwrap();
        assert_eq!(one["deletable_now"], true);
        let all = preview(db, &request(true)).unwrap();
        assert_eq!(all["deletable_now"], false);
        assert_eq!(all["blockers"]["live_run_count"], 28);
        assert_eq!(all["blockers"]["stopping_run_count"], 27);
        assert_eq!(all["blockers"]["runs"].as_array().unwrap().len(), 50);
        assert_eq!(all["blockers"]["truncated"], true);
        db.execute("UPDATE run SET state='completed'", [])?;
        // Preview reserves nothing: a fresh query sees the updated state.
        assert_eq!(preview(db, &request(true)).unwrap()["deletable_now"], true);
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn deleted_parent_does_not_bridge_descendants_and_deleted_target_is_unavailable() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        thread(db, "parent", None);
        thread(db, "deleted", Some("parent"));
        thread(db, "grandchild", Some("deleted"));
        db.execute("UPDATE thread SET deleted_at='now' WHERE id='deleted'", [])?;
        let p = preview(db, &request(true)).unwrap();
        assert_eq!(p["selection"]["thread_count"], 1);
        assert_eq!(p["selection"]["descendant_thread_count"], 0);
        for target in ["missing", "deleted"] {
            assert_eq!(
                preview(
                    db,
                    &ThreadDeleteArgs {
                        thread_id: target.into(),
                        descendants: true
                    }
                )
                .unwrap_err()
                .code,
                ErrorCode::ThreadNotFound
            );
        }
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_gc_keeps_shared_and_summary_source_and_pin_after_path_removal() {
    let (dir, db) = fixture().await;
    db.call(|db| {
        thread(db,"parent",None); thread(db,"survivor",None);
        for id in ["shared","source","private","summary","pinned"] {node(db,id);}
        db.execute_batch("INSERT INTO thread_path VALUES('parent',0,'shared'),('parent',1,'source'),('parent',2,'private'),('parent',3,'pinned'),('survivor',0,'shared'),('survivor',1,'summary');
            INSERT INTO replacement VALUES('rep','summary','key','1','1','completed','now');
            INSERT INTO replacement_source VALUES('rep',0,'source',0,'hash');
            INSERT INTO runtime_pin VALUES('pin','fixture','node','pinned',0,'now');")?;
        let p=preview(db,&request(false)).unwrap();
        assert_eq!(p["references"]["shared_path_nodes"],1);
        assert_eq!(p["references"]["surviving_replacement_source_nodes"],1);
        assert!(p["reclaimable_bytes"].is_null()); // runtime pins are outside these direct reference counts
        db.execute_batch("UPDATE thread SET deleted_at='now' WHERE id='parent'; DELETE FROM thread_path WHERE thread_id='parent';
            UPDATE app_meta SET value=CAST(value AS INTEGER)+1 WHERE key='root_revision';
            INSERT INTO gc_job VALUES('job','history',(SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='root_revision'),NULL,'pending',NULL);")?;
        Ok(())
    }).await.unwrap();
    let config = Arc::new(Config::default());
    let attachments = Arc::new(AttachmentService::new(
        db.clone(),
        &config,
        dir.path().to_owned(),
        "fixture".into(),
    ));
    let gc = GcService::new(db.clone(), config, dir.path().to_owned(), attachments);
    assert_eq!(gc.start_history().await.unwrap().as_deref(), Some("job"));
    let mut completed = false;
    for _ in 0..20 {
        if gc.history_step("job").await.unwrap().complete {
            completed = true;
            break;
        }
    }
    assert!(completed);
    db.call(|db| {
        for id in ["shared", "source", "summary", "pinned"] {
            assert!(db.query_row(
                "SELECT payload_json IS NOT NULL FROM node WHERE id=?1",
                [id],
                |r| r.get::<_, bool>(0)
            )?);
        }
        assert_eq!(
            db.query_row(
                "SELECT deleted_reason FROM node WHERE id='private'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "thread_deleted_unreachable"
        );
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn caller_snapshot_stays_consistent_while_another_connection_changes_blocker() {
    let (dir, db) = fixture().await;
    let path = dir.path().join("deletion.db");
    db.call(move |db| {
        thread(db, "parent", None);
        let tx = db.transaction()?;
        // Establish the read snapshot before a concurrent writer commits.
        let initial = preview(&tx, &request(false)).unwrap();
        assert_eq!(initial["deletable_now"], true);
        let writer = Connection::open(path)?;
        writer.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES('new-run','parent','live','now')", [])?;
        assert_eq!(preview(&tx, &request(false)).unwrap()["deletable_now"], true);
        tx.rollback()?;
        assert_eq!(preview(db, &request(false)).unwrap()["deletable_now"], false);
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}
