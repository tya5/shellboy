use serde_json::json;
use shellboy::{
    attachments::{AddPlan, PlannedAttachment},
    config::Config,
    control::db::Store,
    history,
    protocol::*,
    storage::DbWorker,
};
use std::sync::Arc;
async fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let db = DbWorker::open(root.path().join("db")).await.unwrap();
    let config=Config::parse("default_connection=\"p\"\ndefault_model=\"m\"\n[connections.p]\nbase_url=\"http://127.0.0.1:9/v1\"\n").unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    db.call(move|db|{db.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('thread',?1,?1,'p','m','now')",[cwd])?;Ok(())}).await.unwrap();
    let store = Store::new(db, Arc::new(config), root.path().into(), "instance".into());
    (root, store)
}
fn attachment_path() -> std::path::PathBuf {
    std::env::temp_dir()
        .join("shellboy-attachment-context")
        .join("external.bin")
}
fn request() -> Request {
    Request::new(
        "instance:attach",
        Command::AttachmentAdd(AttachmentAddArgs {
            thread_id: Some("thread".into()),
            path: attachment_path().to_str().unwrap().into(),
            kind: AttachmentKind::File,
        }),
    )
}
fn plan(revision: i64) -> AddPlan {
    AddPlan {
        thread_id: "thread".into(),
        selection_revision: revision,
        files: vec![PlannedAttachment {
            path: attachment_path(),
            kind: AttachmentKind::File,
            observed_bytes: Some(5),
            managed_id: None,
        }],
    }
}
#[tokio::test]
async fn human_cli_attachment_is_anchored_to_its_own_immutable_event_once_without_a_run() {
    let (_root, store) = fixture().await;
    let result = store
        .execute_with_plan(request(), None, plan(0))
        .await
        .unwrap();
    assert!(result.wake_threads.is_empty());
    assert!(result.quiet_wake_threads.is_empty());
    // A repeated committed operation must not append another history event.
    store
        .execute_with_plan(request(), None, plan(2))
        .await
        .unwrap();
    store.database.call(|db|{let(id,body):(String,String)=db.query_row("SELECT n.id,n.payload_json FROM attachment_ref a JOIN node n ON n.id=a.source_node_id",[],|r|Ok((r.get(0)?,r.get(1)?)))?;assert!(body.contains("attachment_ref")&&body.contains("external_cli"));assert_eq!(db.query_row("SELECT COUNT(*) FROM thread_path WHERE node_id=?1",[id],|r|r.get::<_,i64>(0))?,1);assert_eq!(db.query_row("SELECT COUNT(*) FROM node",[],|r|r.get::<_,i64>(0))?,1);assert_eq!(db.query_row("SELECT COUNT(*) FROM run",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).await.unwrap();
    store
        .database
        .call(|db| {
            db.execute(
                "INSERT INTO app_meta VALUES('index.observed.thread','{}')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .execute(
            Request::new(
                "instance:delete",
                Command::ThreadDelete(ThreadDeleteArgs {
                    thread_id: "thread".into(),
                    descendants: false,
                }),
            ),
            None,
        )
        .await
        .unwrap();
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM app_meta WHERE key='index.observed.thread'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn authenticated_cli_reference_uses_dedicated_pending_node_and_a_stale_plan_leaves_no_event()
{
    let (_root, store) = fixture().await;
    store.database.call(|db|{let tx=db.transaction()?;tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','running','now'); INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','turn',0,0,'p','m',0,'completed','{}','now');")?;history::append_node(&tx,"thread","assistant",Some("turn"),"assistant_response",&json!({"output":[{"type":"function_call","call_id":"call","name":"shell","arguments":"{}"}],"group_id":"group"}),"now")?;tx.execute_batch("INSERT INTO output_item VALUES('attempt',0,NULL,'function_call',NULL,'completed','{}','assistant'); INSERT INTO tool_group VALUES('group','attempt','turn','running','now'); INSERT INTO tool_call VALUES('group','call',0,'shell','{}','running','execution'); INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('execution','run','turn','execution','running','/','{}','UTF-8','UTF-8','closed','now');")?;tx.commit()?;Ok(())}).await.unwrap();
    let origin = Origin {
        instance_id: "instance".into(),
        thread_id: "thread".into(),
        run_id: "run".into(),
        turn_id: "turn".into(),
        execution_id: "execution".into(),
        token: "fixture-validated".into(),
    };
    let result = store
        .execute_with_plan(request(), Some(origin), plan(1))
        .await
        .unwrap();
    assert!(result.wake_threads.is_empty());
    assert_eq!(result.quiet_wake_threads, vec!["thread"]);
    store
        .database
        .call(|db| {
            assert_ne!(
                db.query_row("SELECT source_node_id FROM attachment_ref", [], |r| r
                    .get::<_, String>(0))?,
                "assistant"
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM thread_path", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT status FROM attachment_delivery", [], |r| r
                    .get::<_, String>(0))?,
                "accepted"
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM node", [], |r| r.get::<_, i64>(0))?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    let stale = Request::new("instance:stale", request().command);
    assert_eq!(
        store
            .execute_with_plan(stale, None, plan(0))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidState
    );
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM node", [], |r| r.get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM attachment_ref", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
