//! Completion decisions after request-start admission closes are still ordinary
//! completion for Runs without waiting Revisit messages.
use super::*;
use crate::app::DataOwner;

#[tokio::test]
async fn closed_start_gate_only_defers_a_waiting_message_and_preserves_child_completion() {
    for waiting in [false, true] {
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
            if waiting {
                db.execute_batch("INSERT INTO node(id,kind,created_at,payload_json)VALUES('waiting','user','now','{}'); INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,created_at,hold_reason,result_json)VALUES('receipt','fixture:waiting','null','child','waiting','held','now','run_completion','{\"deliver\":\"complete\"}');")?;
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
            assert_eq!(state,if waiting{"live"}else{"completed"});
            assert!(reason.is_none(),"a normal completion must not acquire app_shutdown");
            let events:i64=db.query_row("SELECT count(*) FROM event WHERE kind='child_run_ended' AND target_run_id='parent-run' AND json_extract(payload_ref_json,'$.state')='completed'",[],|r|r.get(0))?;
            assert_eq!(events,if waiting{0}else{1});
            if waiting {
                let saved:(String,Option<String>)=db.query_row("SELECT state,run_id FROM receipt WHERE id='receipt'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
                assert_eq!(saved,("held".into(),None),"no Turn starts after Quit");
            }
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
