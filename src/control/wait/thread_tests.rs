use super::*;
use crate::app::DataOwner;

fn args(thread: &str, after: Option<String>) -> WaitArgs {
    WaitArgs {
        conditions: vec![WaitCondition::ThreadSettled {
            thread_ids: vec![thread.into()],
            states: vec![
                "completed".into(),
                "recovery_wait".into(),
                "stopped".into(),
                "failed".into(),
            ],
        }],
        timeout_ms: 0,
        after,
    }
}
async fn open(root: &std::path::Path) -> DataOwner {
    let path = root.join("config.toml");
    std::fs::write(&path, "").unwrap();
    DataOwner::start(Some(path), Some(root.join("data")), root.into())
        .await
        .unwrap()
}
#[tokio::test]
async fn recovery_resume_and_new_run_between_waits_survive_an_owner_restart() {
    crate::test_watchdog::watchdog("Thread wait durable cursor and owner restart",async {
        let root=tempfile::tempdir().unwrap();let owner=open(root.path()).await;let control=Control::new(&owner,root.path().into());
        owner.db.call(|db|{
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','t','/','/a','p','m','now')",[])?;Ok(())
        }).await.unwrap();
        let first=observe(control.clone(),"first".into(),args("t",None),None,CancellationToken::new()).await.unwrap();
        assert_eq!(first["status"],"timeout");let cursor=first["cursor"].as_str().unwrap().to_owned();
        owner.db.call(|db|{
            let tx=db.transaction()?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('old','t','completed','2000','2001')",[])?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('active','t','live','2002')",[])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','active','running','2002')",[])?;
            assert!(crate::control::state::recover_turn_tx(&tx,"t","active","u",0,&ApiError::new(ErrorCode::IoFailed,"fixture"))?);
            assert!(crate::control::state::resume_recovery_turn_tx(&tx,"t","active","u",0,"2003")?);
            tx.commit()?;Ok(())
        }).await.unwrap();
        let result=observe(control.clone(),"next".into(),args("t",Some(cursor)),None,CancellationToken::new()).await.unwrap();
        assert_eq!(result["status"],"satisfied");assert_eq!(result["snapshot"]["changes"].as_array().unwrap().len(),2);
        assert!(result["snapshot"]["changes"].as_array().unwrap().iter().any(|c|c["state"]=="recovery_wait"));
        let cursor=result["cursor"].as_str().unwrap().to_owned();
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
        let owner=open(root.path()).await;let control=Control::new(&owner,root.path().into());
        let result=observe(control.clone(),"after-restart".into(),args("t",Some(cursor)),None,CancellationToken::new()).await.unwrap();
        assert_eq!(result["status"],"satisfied");assert!(result["snapshot"]["changes"].as_array().unwrap().iter().any(|c|c["state"]=="stopped"));
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn registered_thread_wait_retains_its_baseline_when_completion_is_followed_by_a_new_run() {
    crate::test_watchdog::watchdog("registered Thread wait and transient completion",async {
        let root=tempfile::tempdir().unwrap();let owner=open(root.path()).await;let control=Control::new(&owner,root.path().into());
        owner.db.call(|db|{
            db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('parent','parent','/','/a','p','m','now'),('child','child','/','/a','p','m','now');INSERT INTO run(id,thread_id,state,created_at)VALUES('parent-run','parent','live','2000');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('parent-turn','parent','parent-run','running','2000');INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('caller','parent-run','parent-turn','op','running','/','{}','UTF-8','UTF-8','open','2000');")?;
            Ok(())
        }).await.unwrap();
        let mut events=control.subscribe();
        let watcher=control.clone();
        let origin=Origin {instance_id:owner.instance_id.clone(),thread_id:"parent".into(),run_id:"parent-run".into(),turn_id:"parent-turn".into(),execution_id:"caller".into(),token:"fixture".into()};
        let mut query=args("child",None);query.timeout_ms=crate::test_watchdog::WATCHDOG.as_millis() as u64;
        let waiting=tokio::spawn(observe(watcher,"registered".into(),query,Some(origin),CancellationToken::new()));
        // The existing Changed publication follows pending-save and graph registration.
        loop {
            match events.recv().await {
                Ok(crate::control::EngineEvent::Changed{thread_id,..}) if thread_id=="parent"=>break,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{},
                Err(error)=>panic!("registration notification lost: {error}"),
            }
        }
        owner.db.call(|db| {
            let tx=db.transaction()?;
            tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('finished','child','completed','2001','2002');INSERT INTO run(id,thread_id,state,created_at)VALUES('new-live','child','live','2003');")?;
            tx.commit()?;Ok(())
        }).await.unwrap();
        control.changed("child");
        let result=waiting.await.unwrap().unwrap();
        assert_eq!(result["status"],"satisfied");assert_eq!(result["snapshot"]["changes"][0]["run_id"],"finished");
        owner.db.call(|db|{assert_eq!(db.query_row("SELECT state FROM run WHERE id='new-live'",[],|r|r.get::<_,String>(0))?,"live");Ok(())}).await.unwrap();
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn thread_targets_keep_self_ancestor_and_composed_wait_cycle_protection() {
    crate::test_watchdog::watchdog("Thread wait ownership cycles",async {
        let root=tempfile::tempdir().unwrap();let owner=open(root.path()).await;let control=Control::new(&owner,root.path().into());
        owner.db.call(|db| {
            db.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('p','p','/','/a','p','m','now'),('c','c','/','/a','p','m','now');INSERT INTO run(id,thread_id,state,created_at)VALUES('pr','p','live','0');INSERT INTO run(id,thread_id,owner_run_id,state,created_at)VALUES('cr','c','pr','live','0');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('cu','c','cr','running','0');INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('cs','cr','cu','op','running','/','{}','UTF-8','UTF-8','open','0');")?;Ok(())
        }).await.unwrap();
        let origin=Origin {instance_id:owner.instance_id.clone(),thread_id:"c".into(),run_id:"cr".into(),turn_id:"cu".into(),execution_id:"cs".into(),token:"fixture".into()};
        for target in ["c","p"] {
            let error=observe(control.clone(),format!("cycle-{target}"),args(target,None),Some(origin.clone()),CancellationToken::new()).await.unwrap_err();
            assert_eq!(error.code,ErrorCode::WaitCycle);
        }
        assert!(control.waits.entries.lock().unwrap().is_empty());
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}

#[tokio::test]
async fn cancelling_a_thread_observation_keeps_its_target_live_and_removes_pending_metadata() {
    crate::test_watchdog::watchdog("cancelled Thread observation cleanup",async {
        let root=tempfile::tempdir().unwrap();let owner=open(root.path()).await;let control=Control::new(&owner,root.path().into());
        owner.db.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now');INSERT INTO run(id,thread_id,state,created_at)VALUES('live','t','live','now');")?;Ok(())}).await.unwrap();
        let stop=CancellationToken::new();stop.cancel();let mut query=args("t",None);query.timeout_ms=crate::test_watchdog::WATCHDOG.as_millis() as u64;
        let result=observe(control.clone(),"cancelled-thread-wait".into(),query,None,stop).await.unwrap();
        assert_eq!(result["status"],"interrupted");assert!(result["cursor"].is_string());
        owner.db.call(|db|{assert_eq!(db.query_row("SELECT state FROM run WHERE id='live'",[],|r|r.get::<_,String>(0))?,"live");assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key='wait.pending.cancelled-thread-wait'",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).await.unwrap();
        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}
