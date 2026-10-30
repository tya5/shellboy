//! Receipt acceptance and compaction fences without asynchronous driver races.
use super::*;
use crate::{app::DataOwner, control::Control, protocol::*};

#[tokio::test]
async fn accepted_user_message_fences_new_owner_scope_without_revising_selection() {
    for scope in ["new_run", "new_turn", "same_turn"] {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\nresponse_max_retries=0\n").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let compactor =
            Compactor::new(control.store.database.clone(), control.store.config.clone());
        // Store admission returns effects but does not deliver them to a driver.
        // The accepted input must still be off-path at every assertion below.
        let created = control
            .store
            .execute(
                Request::new(
                    format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                    Command::ThreadCreate(ThreadCreateArgs {
                        agents: None,
                        cwd: None,
                        parent_id: None,
                        connection: None,
                        model: None,
                        effort: None,
                    }),
                ),
                None,
            )
            .await
            .unwrap();
        let thread = created.result["thread_id"].as_str().unwrap().to_owned();
        let target = thread.clone();
        control.store.database.call(move|db| {
            let tx=db.transaction()?;
            for id in ["old","middle","last"] {
                history::append_node(&tx,&target,id,None,"message",&json!({"role":"user","content":id}),"now")?;
            }
            if scope!="new_run" {
                tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('existing-run',?1,'live','now')",[&target])?;
            }
            if scope=="same_turn" {
                tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('existing-turn',?1,'existing-run','running','now')",[&target])?;
            }
            tx.commit()?;Ok(())
        }).await.unwrap();
        let snapshot = compactor.snapshot(&thread, true).await.unwrap();
        assert_eq!(snapshot.run_fence.is_some(), scope != "new_run");
        assert_eq!(snapshot.turn_fence.is_some(), scope == "same_turn");
        let captured_selection = snapshot.revision;
        let target = thread.clone();
        let captured_thread_revision = control
            .store
            .database
            .call(move |db| {
                Ok(
                    db.query_row("SELECT revision FROM thread WHERE id=?1", [target], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        let sent = control
            .store
            .execute(
                Request::new(
                    format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                    Command::MessageSend(MessageSendArgs {
                        now: false,
                        thread_id: Some(thread.clone()),
                        text: "new accepted user instruction".into(),
                    }),
                ),
                None,
            )
            .await
            .unwrap();
        assert!(sent.wake_threads.is_empty());
        assert_eq!(
            sent.quiet_wake_threads.as_slice(),
            std::slice::from_ref(&thread)
        );
        let accepted: MessageAccepted = serde_json::from_value(sent.result).unwrap();
        assert!(!accepted.now);
        let expected_assignment = match scope {
            "new_run" => Assignment::NewRun,
            "new_turn" => Assignment::NewTurn,
            _ => Assignment::ExistingTurn,
        };
        assert_eq!(accepted.assignment, expected_assignment);
        let target = thread.clone();
        let message = accepted.message_id.clone();
        control.store.database.call(move|db| {
            let (revision,selection):(i64,i64)=db.query_row("SELECT revision,selection_revision FROM thread WHERE id=?1",[&target],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert!(revision>captured_thread_revision,"acceptance advances Thread metadata");
            assert_eq!(selection as u64,captured_selection,"receipt acceptance does not claim selected history adoption");
            let (state,admission,selected):(String,String,bool)=db.query_row("SELECT state,path_admission,EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?2 AND node_id=?1) FROM receipt WHERE message_node_id=?1",params![message,target],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!((state.as_str(),admission.as_str(),selected),("accepted","unadmitted",false));
            if scope=="same_turn" {
                assert_eq!(db.query_row("SELECT generation FROM turn WHERE id='existing-turn'",[],|r|r.get::<_,i64>(0))?,0,"ordinary queued input never interrupts current generation");
            }
            Ok(())
        }).await.unwrap();
        let result = compactor
            .omit_range(snapshot, 0..1, CancellationToken::new())
            .await;
        if scope == "same_turn" {
            assert_eq!(
                result.unwrap().omitted_nodes,
                1,
                "same-Turn pending input does not invalidate unchanged selected sources"
            );
        } else {
            assert!(result.unwrap_err().message.contains("snapshot changed"));
        }
        let target = thread.clone();
        let message = accepted.message_id;
        control.store.database.call(move|db| {
            assert_eq!(db.query_row("SELECT count(*) FROM replacement",[],|r|r.get::<_,i64>(0))?,if scope=="same_turn"{1}else{0});
            assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE thread_id=?1 AND node_id IN ('old','middle','last')",[target],|r|r.get::<_,i64>(0))?,if scope=="same_turn"{2}else{3});
            assert_eq!(db.query_row("SELECT count(*) FROM node WHERE id IN ('old','middle','last') AND payload_json IS NOT NULL AND deleted_reason IS NULL",[],|r|r.get::<_,i64>(0))?,3,"all immutable source bodies survive");
            assert_eq!(db.query_row("SELECT state,path_admission,EXISTS(SELECT 1 FROM thread_path WHERE node_id=?1) FROM receipt WHERE message_node_id=?1",[message],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?)))?,("accepted".into(),"unadmitted".into(),false),"compaction never erases or silently admits queued input");
            Ok(())
        }).await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
