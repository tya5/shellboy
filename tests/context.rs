use serde_json::json;
use shellboy::{context, protocol::ErrorCode, storage::DbWorker};

async fn fixture() -> (tempfile::TempDir, DbWorker) {
    let dir = tempfile::tempdir().unwrap();
    let db = DbWorker::open(dir.path().join("context.db")).await.unwrap();
    db.call(|db| {
        db.execute_batch(r#"
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES
                ('origin','/fixture','/fixture/AGENTS.md','proxy','fixture','2026-10-25T00:01:00Z'),
                ('fork','/fixture','/fixture/AGENTS.md','proxy','fixture','2026-10-25T00:02:00Z');
            UPDATE thread SET parent_thread_id='origin' WHERE id='fork';
            INSERT INTO run(id,thread_id,state,created_at,ended_at,stop_reason) VALUES
                ('owner','origin','completed','2026-10-25T00:00:00Z','2026-10-25T00:00:10Z',NULL),
                ('child','fork','stopped','2026-10-25T00:00:01Z','2026-10-25T00:00:04.125Z','explicit_stop');
            UPDATE run SET owner_run_id='owner' WHERE id='child';
            INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at) VALUES
                ('turn','origin','owner','completed','2026-10-25T00:00:01Z','2026-10-25T00:00:03Z');
            INSERT INTO node(id,turn_id,kind,created_at,payload_json) VALUES
                ('node','turn','user','2026-10-25T00:00:01Z','{"text":"synthetic"}'),
                ('held',NULL,'user','2026-10-25T00:00:05Z','{"text":"held"}');
            INSERT INTO thread_path VALUES('origin',0,'node'),('fork',0,'node');
            INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,hold_reason) VALUES
                ('receipt','operation','{"kind":"human"}','origin','held','held','2026-10-25T00:00:05Z','recovery_wait');
        "#)?;
        Ok(())
    }).await.unwrap();
    (dir, db)
}

#[tokio::test]
async fn closed_lifetimes_preserve_saved_times_and_distinguish_parent_from_owner() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        let child = context::for_run(db, "child").unwrap();
        assert_eq!(child.lifetime_ms, Some(3125));
        assert_eq!(child.owner_run_id.as_deref(), Some("owner"));
        assert_eq!(child.stop_reason.as_deref(), Some("explicit_stop"));
        assert!(child.started_at.is_none() && child.parent_thread_id.is_none());
        let thread = context::for_thread(db, "fork").unwrap();
        assert_eq!(thread.parent_thread_id.as_deref(), Some("origin"));
        let json = serde_json::to_value(thread).unwrap();
        for absent in [
            "created_at",
            "started_at",
            "ended_at",
            "lifetime_ms",
            "owner_run_id",
        ] {
            assert!(json.get(absent).is_none(), "{absent}");
        }
        let turn = context::for_turn(db, "turn").unwrap();
        assert_eq!(turn.lifetime_ms, Some(2000));
        assert_eq!(turn.created_at.as_deref(), Some("2026-10-25T00:00:01Z"));
        db.execute(
            "UPDATE turn SET ended_at='2026-10-25T00:00:00.999999999Z' WHERE id='turn'",
            [],
        )?;
        assert!(context::for_turn(db, "turn").unwrap().lifetime_ms.is_none());
        db.execute(
            "UPDATE turn SET ended_at=NULL,state='recovery_wait' WHERE id='turn'",
            [],
        )?;
        assert!(context::for_turn(db, "turn").unwrap().lifetime_ms.is_none());
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_reason_is_structured_and_is_not_automatically_a_failure() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        db.execute("UPDATE turn SET state='recovery_wait',ended_at=NULL,recovery_reason_json=?1 WHERE id='turn'",[json!({"code":"proxy_failed","message":"synthetic failure"}).to_string()])?;
        let turn=context::for_turn(db,"turn").unwrap();
        assert_eq!(turn.recovery_reason.as_ref().unwrap()["code"],"proxy_failed");
        assert!(turn.failure_reason.is_none());
        assert_eq!(context::for_run(db,"owner").unwrap().recovery_reason,turn.recovery_reason);
        db.execute("UPDATE turn SET state='failed' WHERE id='turn'",[])?;
        let failed=context::for_turn(db,"turn").unwrap();
        assert_eq!(failed.failure_reason,failed.recovery_reason);
        db.execute("UPDATE turn SET state='stopped',ended_at='2026-10-25T00:00:06Z',recovery_reason_json=?1 WHERE id='turn'",[json!({"reason":"previous_instance_interrupted","result_uncertain":true}).to_string()])?;
        let recovered=context::for_turn(db,"turn").unwrap();
        assert_eq!(recovered.recovery_reason.as_ref().unwrap()["result_uncertain"],true);
        assert!(recovered.failure_reason.is_none());
        db.execute("UPDATE turn SET recovery_reason_json='null' WHERE id='turn'",[])?;
        let unknown=context::for_turn(db,"turn").unwrap();
        assert!(serde_json::to_value(unknown).unwrap().get("recovery_reason").is_none());
        let held=context::for_message(db,"held").unwrap();
        assert_eq!(held.state,"held");
        assert_eq!(held.hold_reason.as_deref(),Some("recovery_wait"));
        assert!(held.updated_at.is_none() && held.run_id.is_none() && held.failure_reason.is_none());
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn tombstone_origin_survives_thread_deletion_and_reads_are_side_effect_free() {
    let (_dir, db) = fixture().await;
    db.call(|db| {
        db.execute_batch("UPDATE thread SET deleted_at='2026-10-25T00:03:00Z' WHERE id='origin'; UPDATE node SET payload_json=NULL,payload_bytes=0,payload_revision=payload_revision+1,deleted_reason='gc_unreachable' WHERE id='node';")?;
        let before=db.total_changes();
        let node=context::for_node(db,"node").unwrap();
        assert_eq!(node.state,"tombstone");
        assert_eq!(node.thread_id.as_deref(),Some("origin"));
        assert_eq!(node.tombstone_reason.as_deref(),Some("gc_unreachable"));
        assert_eq!(node.turn_id.as_deref(),Some("turn"));
        assert_eq!(node.run_id.as_deref(),Some("owner"));
        assert_eq!(context::for_thread(db,"origin").unwrap().state,"deleted");
        let unassigned=context::for_node(db,"held").unwrap();
        assert!(unassigned.thread_id.is_none() && unassigned.turn_id.is_none());
        assert_eq!(context::for_node(db,"missing").unwrap_err().code,ErrorCode::TargetUnavailable);
        assert_eq!(context::for_run(db,"missing").unwrap_err().code,ErrorCode::TargetUnavailable);
        assert_eq!(context::for_node(db,"node").unwrap(),node);
        assert_eq!(db.total_changes(),before);
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
}
