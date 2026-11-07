//! Thread archive (#222 stage 1): organization metadata only. Hidden from the
//! default list, still readable and countable, never changes `updated_at`, and
//! a new Run on an archived Thread makes it visible again.
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
/// Fixed, distinct update times so list order is checkable.
async fn stamp(store: &Store, ids: &[String]) {
    let ids = ids.to_vec();
    store
        .database
        .call(move |db| {
            for (i, id) in ids.iter().enumerate() {
                db.execute(
                    "UPDATE thread SET updated_at=?2 WHERE id=?1",
                    rusqlite::params![id, format!("2026-11-01T00:00:{i:02}Z")],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
}
async fn archive(store: &Store, ids: &[&str], descendants: bool) -> Result<Value, ApiError> {
    run(
        store,
        Command::ThreadArchive(ThreadArchiveArgs {
            thread_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            descendants,
        }),
    )
    .await
}
async fn unarchive(store: &Store, ids: &[&str], descendants: bool) -> Result<Value, ApiError> {
    run(
        store,
        Command::ThreadUnarchive(ThreadArchiveArgs {
            thread_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            descendants,
        }),
    )
    .await
}
async fn list(
    store: &Store,
    archived: ArchivedFilter,
    limit: u32,
    cursor: Option<String>,
) -> Result<Page<Value>, ApiError> {
    run(
        store,
        Command::ThreadList(ThreadListArgs {
            limit,
            cursor,
            query: None,
            archived,
            tags: Vec::new(),
            cwd: None,
        }),
    )
    .await
    .map(|value| serde_json::from_value(value).unwrap())
}
fn ids(page: &Page<Value>) -> Vec<String> {
    page.items
        .iter()
        .map(|item| item["thread_id"].as_str().unwrap().to_owned())
        .collect()
}
async fn column(store: &Store, id: &str, sql: &'static str) -> Option<String> {
    let id = id.to_owned();
    store
        .database
        .call(move |db| {
            Ok(db.query_row(sql, [id], |row| {
                Ok(match row.get::<_, rusqlite::types::Value>(0)? {
                    rusqlite::types::Value::Null => None,
                    rusqlite::types::Value::Integer(value) => Some(value.to_string()),
                    rusqlite::types::Value::Text(value) => Some(value),
                    other => panic!("unexpected column value {other:?}"),
                })
            })?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn archive_and_unarchive_change_only_the_mark_and_never_updated_at() {
    let (_dir, store) = store().await;
    let a = create(&store, None).await;
    stamp(&store, std::slice::from_ref(&a)).await;
    let before_revision = column(&store, &a, "SELECT revision FROM thread WHERE id=?1").await;
    let result = archive(&store, &[&a], false).await.unwrap();
    assert_eq!(
        result,
        json!({"archived":[a],"already":[],"not_found":[],"descendants":false})
    );
    assert_eq!(
        column(&store, &a, "SELECT updated_at FROM thread WHERE id=?1")
            .await
            .as_deref(),
        Some("2026-11-01T00:00:00Z"),
        "organization changes must not move the list order"
    );
    assert_ne!(
        column(&store, &a, "SELECT revision FROM thread WHERE id=?1").await,
        before_revision
    );
    assert!(
        column(&store, &a, "SELECT archived_at FROM thread WHERE id=?1")
            .await
            .is_some()
    );
    // Second archive is a no-op reported as `already`, and revision stays.
    let again = archive(&store, &[&a], false).await.unwrap();
    assert_eq!(again["archived"], json!([]));
    assert_eq!(again["already"], json!([a]));
    let undone = unarchive(&store, &[&a], false).await.unwrap();
    assert_eq!(undone["unarchived"], json!([a]));
    assert!(
        column(&store, &a, "SELECT archived_at FROM thread WHERE id=?1")
            .await
            .is_none()
    );
    assert_eq!(
        column(&store, &a, "SELECT updated_at FROM thread WHERE id=?1")
            .await
            .as_deref(),
        Some("2026-11-01T00:00:00Z")
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn descendants_select_the_current_subtree_and_unknown_ids_are_reported() {
    let (_dir, store) = store().await;
    let root = create(&store, None).await;
    let child = create(&store, Some(&root)).await;
    let grand = create(&store, Some(&child)).await;
    let other = create(&store, None).await;
    // Without --descendants only the named Thread changes.
    let only = archive(&store, &[&root], false).await.unwrap();
    assert_eq!(only["archived"], json!([root]));
    let all = archive(&store, &[&root, "missing"], true).await.unwrap();
    let mut archived: Vec<String> = all["archived"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    archived.sort();
    let mut expected = vec![child.clone(), grand.clone()];
    expected.sort();
    assert_eq!(archived, expected);
    assert_eq!(all["already"], json!([root]));
    assert_eq!(all["not_found"], json!(["missing"]));
    assert!(
        column(&store, &other, "SELECT archived_at FROM thread WHERE id=?1")
            .await
            .is_none()
    );
    // Unarchiving the subtree from the root restores all three.
    let back = unarchive(&store, &[&root], true).await.unwrap();
    assert_eq!(back["unarchived"].as_array().unwrap().len(), 3);
    assert!(
        list(&store, ArchivedFilter::Only, 50, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    // Nothing found at all is an error, not an empty success.
    assert_eq!(
        archive(&store, &["missing"], false).await.unwrap_err().code,
        ErrorCode::ThreadNotFound
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn selection_bounds_refuse_without_changing_anything() {
    let (_dir, store) = store().await;
    let too_many: Vec<String> = (0..201).map(|i| format!("t{i}")).collect();
    let refs: Vec<&str> = too_many.iter().map(String::as_str).collect();
    assert_eq!(
        archive(&store, &refs, false).await.unwrap_err().code,
        ErrorCode::InputLimit
    );
    // 1 root + 256 descendants = 257 Threads: one over the cap.
    let root = create(&store, None).await;
    let r = root.clone();
    store
        .database
        .call(move |db| {
            for i in 0..256 {
                db.execute(
                    "INSERT INTO thread(id,parent_thread_id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,'/','','p','m','now')",
                    rusqlite::params![format!("d{i}"), r],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        archive(&store, &[&root], true).await.unwrap_err().code,
        ErrorCode::InputLimit
    );
    assert!(
        column(&store, &root, "SELECT archived_at FROM thread WHERE id=?1")
            .await
            .is_none()
    );
    let count = store
        .database
        .call(|db| {
            Ok(db.query_row(
                "SELECT count(*) FROM thread WHERE archived_at IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "a refused selection must not archive part of the tree"
    );
    // Exactly 256 is accepted.
    store
        .database
        .call(|db| Ok(db.execute("DELETE FROM thread WHERE id='d0'", [])?))
        .await
        .unwrap();
    let ok = archive(&store, &[&root], true).await.unwrap();
    assert_eq!(ok["archived"].as_array().unwrap().len(), 256);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn list_modes_and_cursors_stay_inside_their_archived_mode() {
    let (_dir, store) = store().await;
    let mut all = Vec::new();
    for _ in 0..5 {
        all.push(create(&store, None).await);
    }
    stamp(&store, &all).await; // all[4] is newest
    archive(&store, &[&all[1], &all[3]], false).await.unwrap();
    let visible = list(&store, ArchivedFilter::Exclude, 50, None)
        .await
        .unwrap();
    assert_eq!(
        ids(&visible),
        [all[4].clone(), all[2].clone(), all[0].clone()]
    );
    let only = list(&store, ArchivedFilter::Only, 50, None).await.unwrap();
    assert_eq!(ids(&only), [all[3].clone(), all[1].clone()]);
    assert!(
        only.items
            .iter()
            .all(|item| item["archived_at"].is_string())
    );
    assert!(
        visible
            .items
            .iter()
            .all(|item| item.get("archived_at").is_none())
    );
    let both = list(&store, ArchivedFilter::Include, 50, None)
        .await
        .unwrap();
    assert_eq!(
        ids(&both),
        [
            all[4].clone(),
            all[3].clone(),
            all[2].clone(),
            all[1].clone(),
            all[0].clone()
        ]
    );
    // Paging inside a mode works and does not skip or repeat.
    let first = list(&store, ArchivedFilter::Only, 1, None).await.unwrap();
    assert_eq!(ids(&first), [all[3].clone()]);
    let second = list(&store, ArchivedFilter::Only, 1, first.next_cursor.clone())
        .await
        .unwrap();
    assert_eq!(ids(&second), [all[1].clone()]);
    assert!(second.next_cursor.is_none());
    // A cursor never crosses modes in either direction.
    for mode in [ArchivedFilter::Exclude, ArchivedFilter::Include] {
        assert_eq!(
            list(&store, mode, 1, first.next_cursor.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::CursorExpired
        );
    }
    let page = list(&store, ArchivedFilter::Exclude, 1, None)
        .await
        .unwrap();
    assert_eq!(
        list(&store, ArchivedFilter::Only, 1, page.next_cursor)
            .await
            .unwrap_err()
            .code,
        ErrorCode::CursorExpired
    );
    // Archiving did not reorder the rest.
    unarchive(&store, &[&all[1], &all[3]], false).await.unwrap();
    let restored = list(&store, ArchivedFilter::Exclude, 50, None)
        .await
        .unwrap();
    assert_eq!(
        ids(&restored),
        all.iter().rev().cloned().collect::<Vec<_>>()
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn archived_threads_stay_readable_countable_and_can_receive_messages() {
    let (_dir, store) = store().await;
    let parent = create(&store, None).await;
    let child = create(&store, Some(&parent)).await;
    archive(&store, &[&child], false).await.unwrap();
    let details = run(
        &store,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(child.clone()),
        }),
    )
    .await
    .unwrap();
    assert!(details["archived_at"].is_string());
    let usage = run(
        &store,
        Command::UsageGet(UsageGetArgs {
            thread_id: Some(parent.clone()),
            include_children: true,
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        usage["selected_threads"], 2,
        "archived children still count in usage"
    );
    let history = run(
        &store,
        Command::HistoryList(HistoryListArgs {
            excerpt_chars: DEFAULT_HISTORY_EXCERPT_CHARS,
            thread_id: child.clone(),
            limit: DEFAULT_LIST_LIMIT,
            cursor: None,
            run_id: None,
            since: None,
            state: None,
        }),
    )
    .await;
    assert!(
        history.is_ok(),
        "archived history stays readable: {history:?}"
    );
    // The filter still finds it when archived Threads are included.
    let found = run(
        &store,
        Command::ThreadList(ThreadListArgs {
            limit: 10,
            cursor: None,
            query: Some(child.clone()),
            archived: ArchivedFilter::Include,
            tags: Vec::new(),
            cwd: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    // And it can still receive a message (which starts a Run, so it returns to the list).
    let sent = run(&store, send(&child, "still reachable")).await.unwrap();
    assert!(sent["message_id"].is_string(), "{sent}");
    assert_eq!(sent["assignment"], "new_run");
    store.database.shutdown().await.unwrap();
}

fn send(thread: &str, text: &str) -> Command {
    Command::MessageSend(MessageSendArgs {
        human: false,
        to_human: false,
        expect_reply: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: Some(thread.into()),
        text: text.into(),
    })
}

#[tokio::test]
async fn only_a_new_run_unarchives_and_a_live_run_does_not_block_archiving() {
    let (_dir, store) = store().await;
    let thread = create(&store, None).await;
    stamp(&store, std::slice::from_ref(&thread)).await;
    // Idle archived Thread: a message starts a new Run and unarchives it.
    archive(&store, &[&thread], false).await.unwrap();
    run(&store, send(&thread, "wake up")).await.unwrap();
    assert!(
        column(
            &store,
            &thread,
            "SELECT archived_at FROM thread WHERE id=?1"
        )
        .await
        .is_none(),
        "a new Run returns the Thread to the list"
    );
    let events = store
        .database
        .call({
            let thread = thread.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM event WHERE target_thread_id=?1 AND json_extract(payload_ref_json,'$.kind')='thread_unarchived' AND json_extract(payload_ref_json,'$.change.reason')='run_started'",
                    [thread],
                    |r| r.get::<_, i64>(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(events, 1);
    // The Run is live now: archiving is allowed and the Run keeps running.
    let stamped = column(&store, &thread, "SELECT updated_at FROM thread WHERE id=?1").await;
    let live = archive(&store, &[&thread], false).await.unwrap();
    assert_eq!(live["archived"], json!([thread]));
    assert_eq!(
        column(&store, &thread, "SELECT updated_at FROM thread WHERE id=?1")
            .await
            .as_deref(),
        stamped.as_deref(),
        "archiving, even with a live Run, did not move the list position"
    );
    let state = column(
        &store,
        &thread,
        "SELECT state FROM run WHERE thread_id=?1 ORDER BY created_at DESC LIMIT 1",
    )
    .await;
    assert_eq!(state.as_deref(), Some("live"));
    // A message into that same live Run starts no new Run: still archived.
    run(&store, send(&thread, "more input")).await.unwrap();
    assert!(
        column(
            &store,
            &thread,
            "SELECT archived_at FROM thread WHERE id=?1"
        )
        .await
        .is_some()
    );
    store.database.shutdown().await.unwrap();
}

async fn full(store: &Store, command: Command) -> shellboy::control::db::MutationResult {
    store.execute(request(command), None).await.unwrap()
}

#[tokio::test]
async fn archive_publishes_changed_threads_once_and_replays_without_effects() {
    let (_dir, store) = store().await;
    let a = create(&store, None).await;
    let b = create(&store, None).await;
    let archive_both = request(Command::ThreadArchive(ThreadArchiveArgs {
        thread_ids: vec![a.clone(), b.clone()],
        descendants: false,
    }));
    let first = store.execute(archive_both.clone(), None).await.unwrap();
    assert_eq!(first.changed_threads, [a.clone(), b.clone()]);
    assert!(first.wake_threads.is_empty() && first.quiet_wake_threads.is_empty());
    // The same operation ID returns the saved result and repeats no effect.
    let replay = store.execute(archive_both, None).await.unwrap();
    assert_eq!(replay.result, first.result);
    assert!(replay.changed_threads.is_empty() && replay.wake_threads.is_empty());
    // A no-op archive under a new operation changes nothing to publish.
    let again = full(
        &store,
        Command::ThreadArchive(ThreadArchiveArgs {
            thread_ids: vec![a.clone()],
            descendants: false,
        }),
    )
    .await;
    assert!(again.changed_threads.is_empty());
    let undone = full(
        &store,
        Command::ThreadUnarchive(ThreadArchiveArgs {
            thread_ids: vec![a.clone()],
            descendants: false,
        }),
    )
    .await;
    assert_eq!(undone.changed_threads, [a]);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn auto_unarchive_by_send_and_deliver_complete_wakes_the_new_run() {
    let (_dir, store) = store().await;
    for deliver in [Deliver::Next, Deliver::Complete] {
        let thread = create(&store, None).await;
        archive(&store, &[&thread], false).await.unwrap();
        let result = full(
            &store,
            Command::MessageSend(MessageSendArgs {
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                deliver,
                thread_id: Some(thread.clone()),
                text: "start".into(),
            }),
        )
        .await;
        assert_eq!(result.result["assignment"], "new_run", "{deliver:?}");
        assert!(
            column(
                &store,
                &thread,
                "SELECT archived_at FROM thread WHERE id=?1"
            )
            .await
            .is_none(),
            "{deliver:?} on an idle Thread starts a Run and unarchives"
        );
    }
    // Against a live Run, --deliver complete is only held: still archived.
    let busy = create(&store, None).await;
    run(&store, send(&busy, "first")).await.unwrap();
    archive(&store, &[&busy], false).await.unwrap();
    let held = full(
        &store,
        Command::MessageSend(MessageSendArgs {
            human: false,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Complete,
            thread_id: Some(busy.clone()),
            text: "after the run".into(),
        }),
    )
    .await;
    assert_ne!(held.result["assignment"], "new_run");
    assert!(
        column(&store, &busy, "SELECT archived_at FROM thread WHERE id=?1")
            .await
            .is_some()
    );
    store.database.shutdown().await.unwrap();
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
async fn a_reply_that_starts_a_run_on_an_archived_sender_unarchives_it() {
    let (_dir, store) = store().await;
    let sender = create(&store, None).await;
    let recipient = create(&store, None).await;
    // The sender has a live Run so it can send as a model.
    let started = run(&store, send(&sender, "work")).await.unwrap();
    let run_id = started["run_id"].as_str().unwrap().to_owned();
    let turn_id = started["turn_id"].as_str().unwrap().to_owned();
    let asked = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                human: false,
                to_human: false,
                expect_reply: true,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(recipient.clone()),
                text: "please answer".into(),
            })),
            Some(origin(&sender, &run_id, &turn_id)),
        )
        .await
        .unwrap()
        .result;
    let message = asked["message_id"].as_str().unwrap().to_owned();
    store
        .database
        .call({
            let (message, run_id) = (message.clone(), run_id.clone());
            move |db| {
                db.execute("UPDATE receipt SET state='applied',path_admission='admitted' WHERE message_node_id=?1", [&message])?;
                // The sender's Run has ended: the reply has to start a new one.
                db.execute("UPDATE run SET state='completed',ended_at='now' WHERE id=?1", [&run_id])?;
                db.execute("UPDATE turn SET state='completed',ended_at='now' WHERE run_id=?1", [&run_id])?;
                Ok(())
            }
        })
        .await
        .unwrap();
    archive(&store, &[&sender], false).await.unwrap();
    let _reply = full(
        &store,
        Command::MessageReply(MessageReplyArgs {
            message_id: message,
            text: "answer".into(),
            sender_label: None,
        }),
    )
    .await;
    assert!(
        column(
            &store,
            &sender,
            "SELECT archived_at FROM thread WHERE id=?1"
        )
        .await
        .is_none(),
        "the reply started a Run on the archived sender"
    );
    store.database.shutdown().await.unwrap();
}
