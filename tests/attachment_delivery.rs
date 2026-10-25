//! Attachment acceptance is separate from immutable selected-path adoption.
use rusqlite::params;
use serde_json::{Value, json};
use shellboy::{
    attachments::{AddPlan, PlannedAttachment},
    config::Config,
    control::{
        attachment_delivery::{apply_pending_tx, has_pending_tx, hold_run_tx},
        db::Store,
    },
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
fn files() -> Vec<PlannedAttachment> {
    vec![PlannedAttachment {
        path: "/fixture/a.txt".into(),
        kind: AttachmentKind::File,
        observed_bytes: Some(1),
        managed_id: None,
    }]
}
async fn accept(store: &Store, op: &str, revision: i64) -> Value {
    store
        .execute_with_plan(
            Request::new(
                format!("instance:{op}"),
                Command::AttachmentAdd(AttachmentAddArgs {
                    thread_id: Some("thread".into()),
                    path: "/fixture/a.txt".into(),
                    kind: AttachmentKind::File,
                }),
            ),
            None,
            AddPlan {
                thread_id: "thread".into(),
                selection_revision: revision,
                files: files(),
            },
        )
        .await
        .unwrap()
        .result
}
async fn apply(store: &Store) -> usize {
    store
        .database
        .call(|db| {
            let tx = db.transaction()?;
            let n = apply_pending_tx(&tx, "thread", "later").unwrap();
            tx.commit()?;
            Ok(n)
        })
        .await
        .unwrap()
}
#[tokio::test]
async fn pending_attachment_waits_for_complete_tool_group_and_never_creates_human_receipt() {
    let (_root, store) = fixture().await;
    store.database.call(|db|{db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','live','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','running','now');INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','turn',0,0,'p','m',0,'completed','{}','now');INSERT INTO tool_group VALUES('group','attempt','turn','running','now');")?;Ok(())}).await.unwrap();
    let result = accept(&store, "pending", 0).await;
    let source = result[0]["source_node_id"].as_str().unwrap().to_owned();
    assert_eq!(result[0]["delivery_status"], "accepted");
    assert_eq!(apply(&store).await, 0);
    let source2 = source.clone();
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            let payload: String = db.query_row(
                "SELECT payload_json FROM node WHERE id=?1",
                [source2],
                |r| r.get(0),
            )?;
            let payload: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["role"], "user");
            assert!(
                payload["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("auxiliary attachment operation")
            );
            assert_eq!(payload["content"][1]["type"], "attachment_ref");
            db.execute("UPDATE tool_group SET state='completed'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(apply(&store).await, 1);
    assert_eq!(apply(&store).await, 0);
    let replay = accept(&store, "pending", 2).await;
    assert_eq!(replay, result);
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT node_id FROM thread_path", [], |r| r
                    .get::<_, String>(0))?,
                source
            );
            assert_eq!(
                db.query_row("SELECT status FROM attachment_delivery", [], |r| r
                    .get::<_, String>(0))?,
                "applied"
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn recovery_and_stopping_hold_and_fork_does_not_copy_unapplied_attachment() {
    let (_root, store) = fixture().await;
    store.database.call(|db|{db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','live','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','recovery_wait','now');")?;let tx=db.transaction()?;shellboy::history::append_node(&tx,"thread","base",None,"user",&json!({"role":"user","content":[{"type":"input_text","text":"base"}]}),"now")?;tx.commit()?;Ok(())}).await.unwrap();
    let accepted = accept(&store, "held", 1).await;
    assert_eq!(accepted[0]["delivery_status"], "held");
    assert_eq!(apply(&store).await, 0);
    store
        .execute(
            Request::new(
                "instance:fork",
                Command::ThreadFork(ThreadForkArgs {
                    thread_id: "thread".into(),
                    node_id: "base".into(),
                    cwd: None,
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
                    "SELECT count(*) FROM attachment_selection WHERE thread_id<>'thread'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            db.execute("UPDATE run SET state='stopping'", [])?;
            db.execute("UPDATE turn SET state='stopped'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(apply(&store).await, 0);
    store
        .database
        .call(|db| {
            db.execute("UPDATE run SET state='live'", [])?;
            db.execute("UPDATE turn SET state='preparing'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(apply(&store).await, 1);
    store.database.shutdown().await.unwrap();
}
#[tokio::test]
async fn draft_source_exists_with_ordered_refs_in_same_acceptance_transaction() {
    let (_root, store) = fixture().await;
    let result = store
        .send_draft(
            "instance:draft".into(),
            ThreadCreateArgs {
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            },
            Some("thread".into()),
            "before image".into(),
            files(),
            Some(0),
        )
        .await
        .unwrap();
    let message = result.result["message_id"].as_str().unwrap().to_owned();
    store.database.call(move|db|{let(source,body):(String,String)=db.query_row("SELECT a.source_node_id,n.payload_json FROM attachment_ref a JOIN node n ON n.id=a.source_node_id",[],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(source,message);let payload:Value=serde_json::from_str(&body).unwrap();assert_eq!(payload["content"][0]["text"],"before image");assert_eq!(payload["content"][1]["type"],"attachment_ref");assert_eq!(db.query_row("SELECT count(*) FROM attachment_ref WHERE source_node_id IS NULL",[],|r|r.get::<_,i64>(0))?,0);let bytes:i64=db.query_row("SELECT payload_bytes FROM node WHERE id=?1",params![message],|r|r.get(0))?;assert_eq!(bytes,body.len()as i64);Ok(())}).await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopping_holds_existing_acceptance_and_thread_deletion_cancels_without_adopting() {
    let (_root, store) = fixture().await;
    store.database.call(|db|{db.execute_batch("INSERT INTO run(id,thread_id,state,created_at)VALUES('run','thread','live','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','thread','run','running','now');")?;Ok(())}).await.unwrap();
    let accepted = accept(&store, "before-stop", 0).await;
    assert_eq!(accepted[0]["delivery_status"], "accepted");
    store
        .execute(
            Request::new(
                "instance:stop",
                Command::RunStop(RunTarget {
                    run_id: "run".into(),
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
                db.query_row("SELECT status FROM attachment_delivery", [], |r| r
                    .get::<_, String>(0))?,
                "held"
            );
            db.execute("UPDATE run SET state='stopped'", [])?;
            db.execute("UPDATE turn SET state='stopped'", [])?;
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
                db.query_row("SELECT status FROM attachment_delivery", [], |r| r
                    .get::<_, String>(0))?,
                "cancelled"
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread_path", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn foreign_shell_origin_waits_for_its_own_group_even_when_target_is_idle() {
    let (_root, store) = fixture().await;
    store.database.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('origin','/','/AGENTS.md','p','m','now');INSERT INTO run(id,thread_id,state,created_at)VALUES('origin-run','origin','live','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('origin-turn','origin','origin-run','running','now');INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('origin-attempt','origin-turn',0,0,'p','m',0,'completed','{}','now');INSERT INTO tool_group VALUES('origin-group','origin-attempt','origin-turn','running','now');INSERT INTO tool_call VALUES('origin-group','call',0,'shell','{}','running','origin-execution');INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('origin-execution','origin-run','origin-turn','origin-execution','running','/','{}','UTF-8','UTF-8','closed','now');")?;Ok(())}).await.unwrap();
    let origin = Origin {
        instance_id: "instance".into(),
        thread_id: "origin".into(),
        run_id: "origin-run".into(),
        turn_id: "origin-turn".into(),
        execution_id: "origin-execution".into(),
        token: "fixture".into(),
    };
    let accepted = store
        .execute_with_plan(
            Request::new(
                "instance:cross",
                Command::AttachmentAdd(AttachmentAddArgs {
                    thread_id: Some("thread".into()),
                    path: "/fixture/a.txt".into(),
                    kind: AttachmentKind::File,
                }),
            ),
            Some(origin),
            AddPlan {
                thread_id: "thread".into(),
                selection_revision: 0,
                files: files(),
            },
        )
        .await
        .unwrap();
    assert_eq!(accepted.result[0]["delivery_status"], "accepted");
    assert_eq!(apply(&store).await, 0);
    let later = accept(&store, "later-external", 1).await;
    assert_eq!(later[0]["delivery_status"], "accepted");
    assert_eq!(
        apply(&store).await,
        0,
        "a later operation must not overtake a blocked origin"
    );
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM thread_path WHERE thread_id='thread'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            db.execute(
                "UPDATE tool_group SET state='completed' WHERE id='origin-group'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(apply(&store).await, 2);
    let expected = vec![
        accepted.result[0]["source_node_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        later[0]["source_node_id"].as_str().unwrap().to_owned(),
    ];
    store
        .database
        .call(move |db| {
            let path = db
                .prepare(
                    "SELECT node_id FROM thread_path WHERE thread_id='thread' ORDER BY ordinal",
                )?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(path, expected);
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn earlier_human_receipt_precedes_cli_source_and_later_human_stays_pending() {
    let (_root, store) = fixture().await;
    let before = store
        .execute(
            Request::new(
                "instance:before",
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some("thread".into()),
                    text: "before attachment".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let attachment = accept(&store, "middle", 0).await;
    let after = store
        .execute(
            Request::new(
                "instance:after",
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some("thread".into()),
                    text: "after attachment".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(apply(&store).await, 1);
    let before = before["message_id"].as_str().unwrap().to_owned();
    let source = attachment[0]["source_node_id"].as_str().unwrap().to_owned();
    let after = after["message_id"].as_str().unwrap().to_owned();
    store
        .database
        .call(move |db| {
            let path: Vec<String> = db
                .prepare(
                    "SELECT node_id FROM thread_path WHERE thread_id='thread' ORDER BY ordinal",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(path, vec![before, source]);
            assert!(!path.contains(&after));
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM receipt WHERE state='accepted'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                2
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopped_run_held_attachment_does_not_move_to_or_block_a_new_run() {
    let (_root, store) = fixture().await;
    let initial = store
        .execute(
            Request::new(
                "instance:initial",
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some("thread".into()),
                    text: "initial".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    let attachment = accept(&store, "old-run", 0).await;
    let run = initial["run_id"].as_str().unwrap().to_owned();
    store
        .execute(
            Request::new(
                "instance:stop-old",
                Command::RunStop(RunTarget {
                    run_id: run.clone(),
                }),
            ),
            None,
        )
        .await
        .unwrap();
    store
        .database
        .call(move |db| {
            db.execute("UPDATE run SET state='stopped' WHERE id=?1", [&run])?;
            db.execute("UPDATE turn SET state='stopped' WHERE run_id=?1", [run])?;
            Ok(())
        })
        .await
        .unwrap();
    let next = store
        .execute(
            Request::new(
                "instance:new-run",
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some("thread".into()),
                    text: "new instruction".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_ne!(initial["run_id"], next["run_id"]);
    assert_eq!(apply(&store).await, 0);
    let source = attachment[0]["source_node_id"].as_str().unwrap().to_owned();
    store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            assert!(!has_pending_tx(&tx, "thread").unwrap());
            assert_eq!(
                tx.query_row(
                    "SELECT status FROM attachment_delivery WHERE node_id=?1",
                    [&source],
                    |r| r.get::<_, String>(0)
                )?,
                "held"
            );
            assert_eq!(
                tx.query_row(
                    "SELECT count(*) FROM thread_path WHERE node_id=?1",
                    [source],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminated_foreign_origin_incomplete_group_does_not_block_live_target() {
    let (_root, store) = fixture().await;
    store
        .execute(
            Request::new(
                "instance:target-start",
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some("thread".into()),
                    text: "target".into(),
                }),
            ),
            None,
        )
        .await
        .unwrap();
    store.database.call(|db|{db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('origin','/','/AGENTS.md','p','m','now');INSERT INTO run(id,thread_id,state,created_at)VALUES('origin-run','origin','live','now');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('origin-turn','origin','origin-run','running','now');INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('origin-attempt','origin-turn',0,0,'p','m',0,'completed','{}','now');INSERT INTO tool_group VALUES('origin-group','origin-attempt','origin-turn','running','now');INSERT INTO tool_call VALUES('origin-group','call',0,'shell','{}','running','origin-execution');INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('origin-execution','origin-run','origin-turn','origin-execution','running','/','{}','UTF-8','UTF-8','closed','now');")?;Ok(())}).await.unwrap();
    let origin = Origin {
        instance_id: "instance".into(),
        thread_id: "origin".into(),
        run_id: "origin-run".into(),
        turn_id: "origin-turn".into(),
        execution_id: "origin-execution".into(),
        token: "fixture".into(),
    };
    store
        .execute_with_plan(
            Request::new(
                "instance:foreign-will-stop",
                Command::AttachmentAdd(AttachmentAddArgs {
                    thread_id: Some("thread".into()),
                    path: "/fixture/a.txt".into(),
                    kind: AttachmentKind::File,
                }),
            ),
            Some(origin),
            AddPlan {
                thread_id: "thread".into(),
                selection_revision: 0,
                files: files(),
            },
        )
        .await
        .unwrap();
    store
        .database
        .call(|db| {
            let tx = db.transaction()?;
            assert!(has_pending_tx(&tx, "thread").unwrap());
            hold_run_tx(&tx, "origin-run").unwrap();
            tx.execute("UPDATE run SET state='stopped' WHERE id='origin-run'", [])?;
            tx.execute("UPDATE turn SET state='stopped' WHERE id='origin-turn'", [])?;
            assert!(!has_pending_tx(&tx, "thread").unwrap());
            assert_eq!(apply_pending_tx(&tx, "thread", "later").unwrap(), 0);
            assert_eq!(
                tx.query_row("SELECT status FROM attachment_delivery", [], |r| r
                    .get::<_, String>(0))?,
                "held"
            );
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}
