//! Re-parenting (#222 stage 2): `thread set --parent`. Only the parent column and
//! `revision` change; live Runs keep their stored owner; both parents and the moved
//! Thread are reported as changed; cycles and unbounded chains are refused.
use serde_json::{Value, json};
use shellboy::{config::Config, control::db::Store, protocol::*, storage::DbWorker};
use std::sync::Arc;
use tempfile::TempDir;

async fn store() -> (TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("db")).await.unwrap();
    let config = Config::parse(
        r#"
        default_connection="test"
        default_model="fixture"
        [connections.test]
        base_url="http://127.0.0.1:1/v1"
        default_effort="custom"
    "#,
    )
    .unwrap();
    let cwd = directory.path().join("project");
    (
        directory,
        Store::new(database, Arc::new(config), cwd, "instance".into()),
    )
}
fn request(command: Command) -> Request {
    Request::new(format!("instance:{}", uuid::Uuid::new_v4()), command)
}
async fn run(store: &Store, command: Command) -> Result<Value, ApiError> {
    store
        .execute(request(command), None)
        .await
        .map(|result| result.result)
}
async fn create(store: &Store, parent: Option<&str>) -> String {
    let value = run(
        store,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_id: parent.map(str::to_owned),
        }),
    )
    .await
    .unwrap();
    value["thread_id"].as_str().unwrap().to_owned()
}
fn set_parent(thread: &str, parent: &str) -> Command {
    Command::ThreadSet(Box::new(ThreadSetArgs {
        thread_id: Some(thread.into()),
        parent_id: Some(parent.into()),
        ..Default::default()
    }))
}
async fn parent_of(store: &Store, id: &str) -> Option<String> {
    let id = id.to_owned();
    store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT parent_thread_id FROM thread WHERE id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap()
}
async fn text(store: &Store, id: &str, sql: &'static str) -> String {
    let id = id.to_owned();
    store
        .database
        .call(move |db| Ok(db.query_row(sql, [id], |row| row.get::<_, String>(0))?))
        .await
        .unwrap()
}
async fn revision(store: &Store, id: &str) -> i64 {
    text(
        store,
        id,
        "SELECT CAST(revision AS TEXT) FROM thread WHERE id=?1",
    )
    .await
    .parse()
    .unwrap()
}
fn send(thread: &str, text: &str) -> Command {
    Command::MessageSend(MessageSendArgs {
        human: false,
        expect_reply: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: Some(thread.into()),
        text: text.into(),
    })
}
fn origin(thread: &str, run: &str, turn: &str) -> Origin {
    Origin {
        instance_id: "instance".into(),
        thread_id: thread.into(),
        run_id: run.into(),
        turn_id: turn.into(),
        execution_id: "execution-fixture".into(),
        token: "fixture-token".into(),
    }
}

#[tokio::test]
async fn move_and_detach_change_only_the_parent_and_never_updated_at() {
    let (_dir, store) = store().await;
    let (a, b, t) = (
        create(&store, None).await,
        create(&store, None).await,
        create(&store, None).await,
    );
    let stamp = "2026-11-01T00:00:00Z";
    store
        .database
        .call({
            let t = t.clone();
            move |db| {
                db.execute(
                    "UPDATE thread SET updated_at=?2 WHERE id=?1",
                    [t, stamp.into()],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let before = revision(&store, &t).await;
    let attached = store
        .execute(request(set_parent(&t, &a)), None)
        .await
        .unwrap();
    assert_eq!(attached.changed_threads, [t.clone(), a.clone()]);
    assert_eq!(attached.result["parent_id"], json!(a));
    assert_eq!(
        attached.result["parent_change"]["previous_parent_id"],
        Value::Null
    );
    assert_eq!(attached.result["parent_change"]["live_run_id"], Value::Null);
    assert!(attached.wake_threads.is_empty());
    assert_eq!(parent_of(&store, &t).await.as_deref(), Some(a.as_str()));
    assert_eq!(revision(&store, &t).await, before + 1);
    assert_eq!(
        text(&store, &t, "SELECT updated_at FROM thread WHERE id=?1").await,
        stamp,
        "organization does not move the list position"
    );
    // Old parent, new parent and the moved Thread are all reported.
    let moved = store
        .execute(request(set_parent(&t, &b)), None)
        .await
        .unwrap();
    assert_eq!(moved.changed_threads, [t.clone(), a.clone(), b.clone()]);
    assert_eq!(
        moved.result["parent_change"]["previous_parent_id"],
        json!(a)
    );
    // Same parent again: nothing to publish, nothing recorded.
    let events = |store: &Store| {
        let t = t.clone();
        let db = store.database.clone();
        async move {
            db.call(move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM event WHERE target_thread_id=?1 AND json_extract(payload_ref_json,'$.kind')='thread_parent'",
                    [t],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap()
        }
    };
    assert_eq!(events(&store).await, 2);
    let same = store
        .execute(request(set_parent(&t, &b)), None)
        .await
        .unwrap();
    assert!(same.changed_threads.is_empty());
    assert_eq!(events(&store).await, 2);
    // An empty string detaches.
    let detached = store
        .execute(request(set_parent(&t, "")), None)
        .await
        .unwrap();
    assert_eq!(detached.changed_threads, [t.clone(), b.clone()]);
    assert!(parent_of(&store, &t).await.is_none());
    assert_eq!(detached.result["parent_id"], Value::Null);
    assert_eq!(
        text(&store, &t, "SELECT updated_at FROM thread WHERE id=?1").await,
        stamp
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_same_operation_id_replays_without_a_second_move() {
    let (_dir, store) = store().await;
    let (a, t) = (create(&store, None).await, create(&store, None).await);
    let op = request(set_parent(&t, &a));
    let first = store.execute(op.clone(), None).await.unwrap();
    let revision_after = revision(&store, &t).await;
    // Another move in between must not be undone or redone by the replay.
    let other = create(&store, None).await;
    run(&store, set_parent(&t, &other)).await.unwrap();
    let replay = store.execute(op, None).await.unwrap();
    assert_eq!(replay.result, first.result);
    assert!(replay.changed_threads.is_empty());
    assert_eq!(parent_of(&store, &t).await.as_deref(), Some(other.as_str()));
    assert_eq!(revision(&store, &t).await, revision_after + 1);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn cycles_missing_and_deleted_parents_are_refused_without_a_change() {
    let (_dir, store) = store().await;
    let a = create(&store, None).await;
    let b = create(&store, Some(&a)).await;
    let c = create(&store, Some(&b)).await;
    for (thread, parent) in [(&a, &a), (&a, &b), (&a, &c), (&b, &c)] {
        let error = run(&store, set_parent(thread, parent)).await.unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidState,
            "{thread} under {parent}"
        );
    }
    assert!(parent_of(&store, &a).await.is_none());
    assert_eq!(parent_of(&store, &b).await.as_deref(), Some(a.as_str()));
    let missing = run(&store, set_parent(&c, "no-such-thread"))
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::ThreadNotFound);
    let gone = create(&store, None).await;
    run(
        &store,
        Command::ThreadDelete(ThreadDeleteArgs {
            preview_marker: None,
            thread_id: gone.clone(),
            descendants: false,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        run(&store, set_parent(&c, &gone)).await.unwrap_err().code,
        ErrorCode::ThreadNotFound
    );
    assert_eq!(
        run(&store, set_parent("no-such-thread", &a))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ThreadNotFound
    );
    // Valid moves inside the tree still work: a subtree can move as a unit.
    let d = create(&store, None).await;
    run(&store, set_parent(&b, &d)).await.unwrap();
    assert_eq!(parent_of(&store, &c).await.as_deref(), Some(b.as_str()));
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn ancestor_walk_is_bounded_and_ends_at_a_stored_cycle() {
    let (_dir, store) = store().await;
    let root = create(&store, None).await;
    let top = create(&store, None).await;
    // A chain just inside the bound, then one just beyond it, built directly.
    let build = |len: usize| {
        let db = store.database.clone();
        let root = root.clone();
        async move {
            db.call(move |db| {
                let mut parent = root;
                let mut last = String::new();
                for i in 0..len {
                    let id = format!("chain-{len}-{i}");
                    db.execute(
                        "INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?2,'/','','test','fixture','now')",
                        rusqlite::params![id, parent],
                    )?;
                    parent = id.clone();
                    last = id;
                }
                Ok(last)
            })
            .await
            .unwrap()
        }
    };
    let short = build(1000).await;
    run(&store, set_parent(&top, &short)).await.unwrap();
    run(&store, set_parent(&top, "")).await.unwrap();
    let long = build(1100).await;
    let error = run(&store, set_parent(&top, &long)).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InputLimit);
    assert!(parent_of(&store, &top).await.is_none());
    // A cycle already stored above the new parent (corrupt data) cannot loop the write.
    store
        .database
        .call(|db| {
            for (id, parent) in [("loop-a", None), ("loop-b", Some("loop-a"))] {
                db.execute(
                    "INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?2,'/','','test','fixture','now')",
                    rusqlite::params![id, parent],
                )?;
            }
            db.execute("UPDATE thread SET parent_thread_id='loop-b' WHERE id='loop-a'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let looped = run(&store, set_parent(&top, "loop-a")).await.unwrap_err();
    assert_eq!(looped.code, ErrorCode::InputLimit);
    assert!(parent_of(&store, &top).await.is_none());
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_live_run_keeps_its_owner_and_the_new_parent_owns_only_the_next_run() {
    let (_dir, store) = store().await;
    let (old, new) = (create(&store, None).await, create(&store, None).await);
    let child = create(&store, Some(&old)).await;
    let started = |thread: &str| {
        let command = send(thread, "work");
        let store = &store;
        async move { run(store, command).await.unwrap() }
    };
    let old_run = started(&old).await;
    let new_run = started(&new).await;
    let old_origin = origin(
        &old,
        old_run["run_id"].as_str().unwrap(),
        old_run["turn_id"].as_str().unwrap(),
    );
    let new_origin = origin(
        &new,
        new_run["run_id"].as_str().unwrap(),
        new_run["turn_id"].as_str().unwrap(),
    );
    // The old parent delegates: the child's Run is owned by the old parent's Run.
    let delegated = store
        .execute(request(send(&child, "delegated")), Some(old_origin.clone()))
        .await
        .unwrap()
        .result;
    let child_run = delegated["run_id"].as_str().unwrap().to_owned();
    let owner = |store: &Store, run: String| {
        let db = store.database.clone();
        async move {
            db.call(move |db| {
                Ok(
                    db.query_row("SELECT owner_run_id FROM run WHERE id=?1", [run], |r| {
                        r.get::<_, Option<String>>(0)
                    })?,
                )
            })
            .await
            .unwrap()
        }
    };
    assert_eq!(
        owner(&store, child_run.clone()).await.as_deref(),
        old_run["run_id"].as_str()
    );
    // Move the busy child: allowed, and the live Run is reported with its old owner.
    let moved = store
        .execute(request(set_parent(&child, &new)), None)
        .await
        .unwrap();
    assert_eq!(
        moved.changed_threads,
        [child.clone(), old.clone(), new.clone()]
    );
    assert_eq!(
        moved.result["parent_change"]["live_run_id"],
        json!(child_run)
    );
    assert_eq!(
        moved.result["parent_change"]["live_run_owner_run_id"],
        old_run["run_id"]
    );
    assert_eq!(
        owner(&store, child_run.clone()).await.as_deref(),
        old_run["run_id"].as_str(),
        "ownership of a live Run is never transferred"
    );
    // The new parent is refused until that Run ends; stopping stays open to any caller.
    let refused = store
        .execute(request(send(&child, "from new")), Some(new_origin.clone()))
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidState);
    let stopped = store
        .execute(
            request(Command::RunStop(RunTarget {
                run_id: child_run.clone(),
            })),
            Some(old_origin.clone()),
        )
        .await
        .unwrap();
    assert_eq!(stopped.stop_runs, std::slice::from_ref(&child_run));
    // Once the Run has ended, the new parent's send starts a Run it owns.
    store
        .database
        .call({
            let run = child_run.clone();
            move |db| {
                db.execute(
                    "UPDATE turn SET state='stopped',ended_at='now' WHERE run_id=?1",
                    [&run],
                )?;
                db.execute(
                    "UPDATE run SET state='stopped',ended_at='now' WHERE id=?1",
                    [&run],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let next = store
        .execute(request(send(&child, "from new")), Some(new_origin))
        .await
        .unwrap()
        .result;
    assert_eq!(next["assignment"], json!("new_run"));
    assert_eq!(
        owner(&store, next["run_id"].as_str().unwrap().into())
            .await
            .as_deref(),
        new_run["run_id"].as_str()
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_pending_reply_still_goes_to_its_sender_after_the_recipient_moves() {
    let (_dir, store) = store().await;
    let (sender, other) = (create(&store, None).await, create(&store, None).await);
    let recipient = create(&store, Some(&sender)).await;
    let started = run(&store, send(&sender, "work")).await.unwrap();
    let from = origin(
        &sender,
        started["run_id"].as_str().unwrap(),
        started["turn_id"].as_str().unwrap(),
    );
    let asked = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                human: false,
                expect_reply: true,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(recipient.clone()),
                text: "answer please".into(),
            })),
            Some(from),
        )
        .await
        .unwrap()
        .result;
    run(&store, set_parent(&recipient, &other)).await.unwrap();
    let message_id = asked["message_id"].as_str().unwrap();
    let state = run(
        &store,
        Command::MessageGet(MessageTarget {
            message_id: message_id.into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(state["reply"]["state"], json!("pending"));
    assert_eq!(state["thread_id"], json!(recipient));
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn usage_and_delete_follow_the_current_tree_and_combined_fields_apply() {
    let (_dir, store) = store().await;
    let (a, b) = (create(&store, None).await, create(&store, None).await);
    let child = create(&store, Some(&a)).await;
    run(&store, set_parent(&child, &b)).await.unwrap();
    // Deleting the old parent's subtree leaves the moved child.
    run(
        &store,
        Command::ThreadDelete(ThreadDeleteArgs {
            preview_marker: None,
            thread_id: a.clone(),
            descendants: true,
        }),
    )
    .await
    .unwrap();
    assert_eq!(parent_of(&store, &child).await.as_deref(), Some(b.as_str()));
    // A move combined with a title change is one transaction and one result.
    let both = run(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(child.clone()),
            parent_id: Some(String::new()),
            title: Some("renamed".into()),
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    assert_eq!(both["title"], json!("renamed"));
    assert!(parent_of(&store, &child).await.is_none());
    // A failing move rolls the other fields back.
    let failed = run(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(child.clone()),
            parent_id: Some(child.clone()),
            title: Some("never".into()),
            ..Default::default()
        })),
    )
    .await;
    assert!(failed.is_err());
    assert_eq!(
        text(&store, &child, "SELECT title FROM thread WHERE id=?1").await,
        "renamed"
    );
    store.database.shutdown().await.unwrap();
}
