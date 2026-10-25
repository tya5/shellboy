use super::spill_and_summarize;
use crate::{compaction::Compactor, config::Config, history, storage::DbWorker};
use rusqlite::params;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

async fn fixture() -> (tempfile::TempDir, DbWorker, Compactor) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("history.db"))
        .await
        .unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    db.call(move |db| {
        db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t',?1,?1,'fixture','fixture','now')",[cwd])?;
        Ok(())
    }).await.unwrap();
    let compactor = Compactor::new(db.clone(), Arc::new(Config::default()));
    (root, db, compactor)
}

#[tokio::test]
async fn latest_group_spill_is_progress_even_when_the_summary_has_no_prefix() {
    let (_root, db, compactor) = fixture().await;
    db.call(|db| {
        let tx = db.transaction()?;
        tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
            INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');
            INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','u',0,0,'fixture','fixture',0,'completed','{}','now');
            INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('g','attempt','u','completed','now');
            INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('g','call',0,'shell','{}','completed');")?;
        history::append_node(&tx,"t","call",Some("u"),"assistant_response",&json!({"group_id":"g","output":[{"type":"function_call","call_id":"call","name":"shell","arguments":"{}"}]}),"now")?;
        let output=json!({"status":"exited","exit_code":0,"stdout":"saved-original-output".repeat(4096)}).to_string();
        history::append_node(&tx,"t","result",Some("u"),"tool_result",&json!({"group_id":"g","item":{"type":"function_call_output","call_id":"call","output":output}}),"now")?;
        tx.execute("INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('g','call','result','completed')",[])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    assert!(compactor.spill("t", false).await.unwrap().no_target);
    let before = db.call(|db| {
        Ok(db.query_row("SELECT SUM(length(CAST(n.payload_json AS BLOB))) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id='t'",[],|r|r.get::<_,i64>(0))?)
    }).await.unwrap();
    let result = spill_and_summarize(&compactor, "t", CancellationToken::new())
        .await
        .unwrap();
    let after = db.call(|db| {
        Ok(db.query_row("SELECT SUM(length(CAST(n.payload_json AS BLOB))) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id='t'",[],|r|r.get::<_,i64>(0))?)
    }).await.unwrap();
    assert!(
        after < before,
        "the real selected payload must shrink: {before} -> {after}"
    );
    assert!(
        !result.no_target,
        "the adopted latest-group reduction must enable retry two"
    );
    assert_eq!(result.spilled_groups, 1);
    assert_eq!(result.original_node_ids, ["call", "result"]);
    db.call(|db| {
        assert_eq!(db.query_row("SELECT COUNT(*) FROM replacement WHERE format_version='spill-v1'", [], |r| r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id='t' AND n.kind='spill'", [], |r| r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM node WHERE id IN ('call','result') AND payload_json IS NOT NULL", [], |r| r.get::<_,i64>(0))?,2);
        let original:String=db.query_row("SELECT payload_json FROM node WHERE id=?1",params!["result"],|r|r.get(0))?;
        assert!(original.contains("saved-original-output"));
        assert_eq!(db.query_row("SELECT COUNT(*) FROM node WHERE kind='summary'", [], |r| r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM shell_execution", [], |r| r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    assert!(
        spill_and_summarize(&compactor, "t", CancellationToken::new())
            .await
            .unwrap()
            .no_target,
        "an already-spilled single group must not invent another reduction"
    );
    db.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_empty_path_does_not_invent_second_stage_progress() {
    let (_root, db, compactor) = fixture().await;
    assert!(
        spill_and_summarize(&compactor, "t", CancellationToken::new())
            .await
            .unwrap()
            .no_target
    );
    db.shutdown().await.unwrap();
}
