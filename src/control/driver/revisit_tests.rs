//! Completion decisions after request-start admission closes are still ordinary
//! completion for Runs without pending Revisit work.
use super::*;
use crate::app::DataOwner;

#[tokio::test]
async fn closed_start_gate_only_defers_a_pending_reservation_and_preserves_child_completion() {
    for pending in [None, Some(false), Some(true)] {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        control.store.database.call(move |db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','/','/AGENTS.md','fixture','fixture','now'),('child','/','/AGENTS.md','fixture','fixture','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','now'); INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES('child-run','child','parent-run','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('child-turn','child','child-run','completed','now','now');")?;
            if let Some(pending) = pending {
                let reservation = pending.then(|| json!({"text":"Do not start after Quit","set_by":{"kind":"human"},"operation_id":"fixture:reserved","set_at":"now"}));
                let state=json!({"pending":reservation,"consumed_count":7,"last_fired_at":"before"});
                db.execute("INSERT INTO app_meta(key,value)VALUES('revisit.child-run',?1)",[state.to_string()])?;
            }
            Ok(())
        }).await.unwrap();
        control.shell.close_start_admission();
        assert!(matches!(
            next(&control, "child", "child-run").await.unwrap(),
            Next::Done
        ));
        control.store.database.call(move|db| {
            let (state,reason):(String,Option<String>)=db.query_row("SELECT state,stop_reason FROM run WHERE id='child-run'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(state,if pending==Some(true){"live"}else{"completed"});
            assert!(reason.is_none(),"a normal completion must not acquire app_shutdown");
            let events:i64=db.query_row("SELECT count(*) FROM event WHERE kind='child_run_ended' AND target_run_id='parent-run' AND json_extract(payload_ref_json,'$.state')='completed'",[],|r|r.get(0))?;
            assert_eq!(events,if pending==Some(true){0}else{1});
            assert_eq!(db.query_row("SELECT count(*) FROM event WHERE kind='revisit'",[],|r|r.get::<_,i64>(0))?,0);
            if pending.is_some() {
                let raw:String=db.query_row("SELECT value FROM app_meta WHERE key='revisit.child-run'",[],|r|r.get(0))?;
                let saved:Value=serde_json::from_str(&raw).unwrap();
                assert_eq!(saved["consumed_count"],7);
                assert_eq!(!saved["pending"].is_null(),pending==Some(true));
            }
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
