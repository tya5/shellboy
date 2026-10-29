//! Real startup transaction cancels interrupted shell attachments before repair.
//! Uses external synthetic files and temporary storage, without HTTP or OS shells.
use rusqlite::params;
use serde_json::json;
use shellboy::{app::DataOwner, control::attachment_delivery, history};
use std::path::Path;
async fn open(root: &Path) -> DataOwner {
    DataOwner::start(
        Some(root.join("config.toml")),
        Some(root.join("data")),
        root.into(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn startup_cancels_unadopted_local_and_cross_thread_shell_sources_before_group_completion() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("config.toml"), "").unwrap();
    let files = [
        "accepted-local",
        "accepted-cross",
        "held-cross",
        "applied-cross",
    ];
    for source in files {
        std::fs::write(
            root.path().join(source),
            format!("preserve external {source}"),
        )
        .unwrap();
    }
    let owner = open(root.path()).await;
    let cwd = root.path().to_string_lossy().into_owned();
    owner.db.call(move|db| {
        let tx=db.transaction()?;
        for thread in ["origin","target"] {tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,'','fixture','fixture','before')",params![thread,cwd])?;}
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('origin-run','origin','live','before')",[])?;
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('origin-turn','origin','origin-run','running','before')",[])?;
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','origin-turn',0,0,'fixture','fixture',0,'completed','{}','before')",[])?;
        let item=json!({"type":"function_call","call_id":"call-origin","name":"shell","arguments":"{\"command\":\"synthetic saved state never executes\"}"});
        history::append_node(&tx,"origin","call-source",Some("origin-turn"),"assistant_response",&json!({"group_id":"group","reasoning_epoch":0,"output":[item]}),"before")?;
        tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('attempt',0,'function_call','completed',?1,'call-source')",[item.to_string()])?;
        tx.execute("INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('group','attempt','origin-turn','running','before')",[])?;
        tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('group','call-origin',0,'shell',?1,'running','origin-shell')",[item["arguments"].as_str().unwrap()])?;
        tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('origin-shell','origin-run','origin-turn','origin-shell','running',?1,'{\"command\":\"synthetic saved metadata\"}','UTF-8','UTF-8','open','before')",[&cwd])?;
        for (source,status,target) in [("accepted-local","accepted","origin"),("accepted-cross","accepted","target"),("held-cross","held","target"),("applied-cross","applied","target")] {
            let payload=json!({"role":"developer","content":[{"type":"input_text","text":format!("synthetic attachment {source}")} ]});
            if status=="applied" {history::append_node(&tx,target,source,None,"attachment_source",&payload,"before")?;} else {let body=payload.to_string();tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'attachment_source','before',?2,?3)",params![source,body,body.len() as i64])?;}
            let path=Path::new(&cwd).join(source).to_string_lossy().into_owned();
            tx.execute("INSERT INTO attachment_ref(id,source_node_id,absolute_path,input_kind,ownership,created_at)VALUES(?1,?1,?2,'file','external','before')",params![source,path])?;
            tx.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at)VALUES(?1,?2,?3,?1,'{\"kind\":\"shell\",\"execution_id\":\"origin-shell\",\"run_id\":\"origin-run\"}','before')",params![source,target,status])?;
            if status=="applied" {tx.execute("INSERT INTO attachment_selection(thread_id,attachment_id,state,selected_at,selected_order)VALUES(?1,?2,'active','before',0)",params![target,source])?;}
        }
        // Assert the order inside the real startup transaction, not merely its
        // final state. Completing the group while these deliveries remain
        // eligible would fail startup with this synthetic invariant trigger.
        tx.execute_batch("CREATE TRIGGER verify_cancel_before_group_complete BEFORE UPDATE OF state ON tool_group WHEN NEW.state='completed' AND OLD.state<>'completed' AND EXISTS(SELECT 1 FROM attachment_delivery d JOIN shell_execution s ON s.id=json_extract(d.origin_json,'$.execution_id') JOIN tool_call c ON c.operation_id=s.id WHERE c.group_id=NEW.id AND s.owner_run_id=json_extract(d.origin_json,'$.run_id') AND d.status IN ('accepted','held')) BEGIN SELECT RAISE(ABORT,'group completed before pending shell attachment cancellation'); END;")?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = open(root.path()).await;
    let snapshot=owner.db.call(|db| {
        assert_eq!(db.query_row("SELECT state FROM tool_group WHERE id='group'",[],|r|r.get::<_,String>(0))?,"completed");
        assert_eq!(db.query_row("SELECT count(*) FROM tool_result WHERE group_id='group'",[],|r|r.get::<_,i64>(0))?,1);
        let states=db.prepare("SELECT node_id,status FROM attachment_delivery ORDER BY node_id")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(states,vec![("accepted-cross".to_owned(),"cancelled".to_owned()),("accepted-local".to_owned(),"cancelled".to_owned()),("applied-cross".to_owned(),"applied".to_owned()),("held-cross".to_owned(),"cancelled".to_owned())]);
        let tx=db.transaction()?;
        assert_eq!(attachment_delivery::apply_pending_tx(&tx,"origin","after").map_err(shellboy::storage::StorageError::from)?,0);
        assert_eq!(attachment_delivery::apply_pending_tx(&tx,"target","after").map_err(shellboy::storage::StorageError::from)?,0);
        assert_eq!(tx.query_row("SELECT count(*) FROM attachment_selection",[],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(tx.query_row("SELECT state FROM attachment_selection WHERE thread_id='target' AND attachment_id='applied-cross'",[],|r|r.get::<_,String>(0))?,"active");
        assert_eq!(tx.query_row("SELECT count(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE n.kind='attachment_source'",[],|r|r.get::<_,i64>(0))?,1,"cancelled sources became selected");
        assert_eq!(tx.query_row("SELECT count(*) FROM node WHERE kind='attachment_source' AND payload_json IS NOT NULL",[],|r|r.get::<_,i64>(0))?,4,"source bodies were deleted");
        assert_eq!(tx.query_row("SELECT count(*) FROM attachment_ref",[],|r|r.get::<_,i64>(0))?,4,"source references were deleted");
        tx.commit()?;Ok(states)
    }).await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = open(root.path()).await;
    let repeated = owner
        .db
        .call(|db| {
            Ok(db
                .prepare("SELECT node_id,status FROM attachment_delivery ORDER BY node_id")?
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        snapshot, repeated,
        "another restart changed attachment disposition"
    );
    for source in files {
        assert_eq!(
            std::fs::read_to_string(root.path().join(source)).unwrap(),
            format!("preserve external {source}")
        );
    }
    owner.shutdown().await.unwrap();
}
