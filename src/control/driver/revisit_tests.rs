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
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','/','/AGENTS.md','fixture','fixture','now'),('child','/','/AGENTS.md','fixture','fixture','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('child-run','child','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('child-turn','child','child-run','completed','now','now');")?;
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

#[tokio::test]
async fn reply_reminder_then_ignored_contains_every_sender() {
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
    control.store.database.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','fixture','fixture','now'); INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('u','t','r','completed','now','now');")?;
        for i in 0..128 {
            let id=format!("message-{i:03}");
            db.execute("INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,'user','now','{}')",[&id])?;
            let sender = if i == 0 {
                json!({"kind":"model","thread_id":"t"})
            } else {
                json!({"kind":"external_cli","label":format!("sender-{i}")})
            };
            db.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,state,path_admission,created_at,result_json)VALUES(?1,?1,?2,'t',?1,'applied','admitted','now','{\"expect_reply\":true,\"reply\":{\"state\":\"pending\"}}')",params![id,sender.to_string()])?;
        }
        Ok(())
    }).await.unwrap();
    control
        .store
        .database
        .call(|db| {
            db.execute("UPDATE run SET state='stopping' WHERE id='r'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        next(&control, "t", "r").await.unwrap(),
        Next::Done
    ));
    control
        .store
        .database
        .call(|db| {
            db.execute("UPDATE run SET state='live' WHERE id='r'", [])?;
            db.execute("UPDATE turn SET state='recovery_wait' WHERE id='u'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        next(&control, "t", "r").await.unwrap(),
        Next::Done
    ));
    control.store.database.call(|db| {
        assert_eq!(db.query_row("SELECT count(*) FROM event WHERE kind='reply_reminder'",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE json_extract(result_json,'$.reply.state')='pending'",[],|r|r.get::<_,i64>(0))?,128);
        db.execute("UPDATE turn SET state='completed' WHERE id='u'",[])?;
        Ok(())
    }).await.unwrap();
    let snapshot = match next(&control, "t", "r").await.unwrap() {
        Next::Ready(s) => s,
        _ => panic!("reminder must continue Run"),
    };
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|id| id.starts_with("reply-reminder-"))
            .count(),
        1,
        "the reminder must be in this continuation request"
    );
    control
        .store
        .database
        .call(move |db| {
            let payload: String = db.query_row(
                "SELECT payload_ref_json FROM event WHERE kind='reply_reminder'",
                [],
                |r| r.get(0),
            )?;
            let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(value["messages"].as_array().unwrap().len(), 128);
            for (i, row) in value["messages"].as_array().unwrap().iter().enumerate() {
                assert_eq!(row["message_id"], format!("message-{i:03}"));
                if i == 0 {
                    assert_eq!(row["sender"]["kind"], "unknown");
                } else {
                    assert_eq!(row["sender"]["label"], format!("sender-{i}"));
                }
            }
            assert!(
                payload.len() < 65536,
                "bounded 128-duty reminder: {}",
                payload.len()
            );
            // Esc completes a Turn without an end message. The next boundary is still normal completion.
            db.execute(
                "UPDATE turn SET state='completed',ended_at='later' WHERE id=?1",
                [snapshot.turn],
            )?;
            db.execute(
                "UPDATE delivery SET state='processed' WHERE recipient_id='r'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        next(&control, "t", "r").await.unwrap(),
        Next::Done
    ));
    control.store.database.call(|db| {
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE json_extract(result_json,'$.reply.state')='ignored'",[],|r|r.get::<_,i64>(0))?,128);
        assert_eq!(db.query_row("SELECT count(*) FROM event WHERE kind='reply_reminder'",[],|r|r.get::<_,i64>(0))?,1);
        Ok(())
    }).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
