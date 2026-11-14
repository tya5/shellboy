use serde_json::Value;
use shellboy::{
    config::Config,
    control::db::{MutationResult, Store},
    protocol::*,
    storage::DbWorker,
};
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
        [connections.other]
        base_url="http://127.0.0.1:2/v1"
        summary_max_chars=12000
        auto_compact_target_percent=3
        [compaction]
        summary_max_chars=10000
    "#,
    )
    .unwrap();
    let startup_cwd = directory.path().join("project");
    (
        directory,
        Store::new(database, Arc::new(config), startup_cwd, "instance".into()),
    )
}
async fn spill_store() -> (TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("db")).await.unwrap();
    let config = Config::parse(
        r#"
        default_connection="off"
        default_model="fixture"
        [connections.off]
        base_url="http://127.0.0.1:1/v1"
        spill=false
        [connections.on]
        base_url="http://127.0.0.1:2/v1"
        spill=true
        [connections.inherit]
        base_url="http://127.0.0.1:3/v1"
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
async fn execute(store: &Store, command: Command) -> MutationResult {
    store.execute(request(command), None).await.unwrap()
}
async fn thread_revisions(store: &Store, id: &str) -> (i64, i64, i64, i64) {
    let id = id.to_owned();
    store.database.call(move |db| {
        Ok(db.query_row(
            "SELECT revision,profile_revision,reasoning_epoch,selection_revision FROM thread WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?)
    }).await.unwrap()
}

async fn create(store: &Store, parent: Option<String>) -> ThreadDetails {
    serde_json::from_value(
        execute(
            store,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: parent,
            }),
        )
        .await
        .result,
    )
    .unwrap()
}
async fn send(store: &Store, thread: &str, text: &str) -> (MessageAccepted, MutationResult) {
    let result = execute(
        store,
        Command::MessageSend(MessageSendArgs {
            to_human: false,
            human: false,
            resume_recovery: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.into()),
            text: text.into(),
        }),
    )
    .await;
    (
        serde_json::from_value(result.result.clone()).unwrap(),
        result,
    )
}
fn caller(thread: &str, run: &str, turn: &str) -> Origin {
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
async fn same_operation_is_durable_idempotent_conflict_checked_and_old_instance_query_only() {
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let req = request(Command::MessageSend(MessageSendArgs {
        to_human: false,
        human: false,
        resume_recovery: false,
        expect_reply: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: Some(thread.thread_id.clone()),
        text: "fixture".into(),
    }));
    let first = store.execute(req.clone(), None).await.unwrap();
    let second = store.execute(req.clone(), None).await.unwrap();
    assert_eq!(first.result, second.result);
    assert!(
        first.wake_threads.is_empty(),
        "ordinary queued send must not release foreground work"
    );
    assert_eq!(first.quiet_wake_threads, vec![thread.thread_id]);
    assert!(
        second.wake_threads.is_empty()
            && second.quiet_wake_threads.is_empty()
            && second.changed_threads.is_empty(),
        "retransmission must not reschedule foreground, quiet or observation effects"
    );
    let mut conflicting = req.clone();
    if let Command::MessageSend(args) = &mut conflicting.command {
        args.text = "different".into();
    }
    assert_eq!(
        store.execute(conflicting, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    store
        .database
        .call(|connection| {
            assert_eq!(
                connection.query_row("SELECT count(*) FROM receipt", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM node", [], |row| row.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                connection.query_row("SELECT count(*) FROM thread_path", [], |row| row
                    .get::<_, i64>(0))?,
                0,
                "receive must not alter a running request's adopted path"
            );
            Ok(())
        })
        .await
        .unwrap();
    let newer = Store::new(
        store.database.clone(),
        store.config.clone(),
        store.startup_cwd.join("elsewhere"),
        "new-instance".into(),
    );
    assert_eq!(newer.execute(req, None).await.unwrap().result, first.result);
    let unknown = Request::new(
        "instance:not-recorded",
        Command::ThreadList(ThreadListArgs::default()),
    );
    assert_eq!(
        newer.execute(unknown, None).await.unwrap().result["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_settings_inherit_agents_and_model_switch_never_restores_old_epoch() {
    let (_directory, store) = store().await;
    let parent = create(&store, None).await;
    let child: ThreadDetails = serde_json::from_value(
        execute(
            &store,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                parent_id: Some(parent.thread_id.clone()),
                cwd: Some("../child".into()),
                connection: None,
                model: None,
                effort: None,
            }),
        )
        .await
        .result,
    )
    .unwrap();
    assert_eq!(
        child.cwd,
        store
            .startup_cwd
            .parent()
            .unwrap()
            .join("child")
            .to_string_lossy()
    );
    assert_eq!(child.effort.as_deref(), Some("custom"));
    let id = child.thread_id.clone();
    let expected_agents = store
        .startup_cwd
        .join("AGENTS.md")
        .to_string_lossy()
        .into_owned();
    for model in ["another", "fixture"] {
        execute(
            &store,
            Command::ThreadSet(Box::new(ThreadSetArgs {
                auto_compact_at: None,
                auto_compact_target_percent: None,
                summary_max_chars: None,
                max_tokens: None,
                spill: None,
                agents: None,
                title: None,
                thread_id: Some(id.clone()),
                connection: None,
                cwd: Some("nested/..".into()),
                model: Some(model.into()),
                effort: None,
                parent_id: None,
            })),
        )
        .await;
    }
    store
        .database
        .call(move |connection| {
            let (agents, epoch, profile_revision): (String, i64, i64) = connection.query_row(
                "SELECT agents_path,reasoning_epoch,profile_revision FROM thread WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(agents, expected_agents);
            assert_eq!(epoch, 2);
            assert_eq!(profile_revision, 2);
            Ok(())
        })
        .await
        .unwrap();
    let missing = store
        .execute(
            request(Command::ThreadGet(ThreadTarget { thread_id: None })),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::InvalidArgument);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn acceptance_assignments_are_immutable_across_completion_stop_and_delete() {
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let (first, _) = send(&store, &thread.thread_id, "first").await;
    assert_eq!(first.assignment, Assignment::NewRun);
    let (second, _) = send(&store, &thread.thread_id, "additional").await;
    assert_eq!(second.assignment, Assignment::ExistingTurn);
    assert_eq!(second.run_id, first.run_id);
    assert_eq!(second.turn_id, first.turn_id);
    let old_turn = first.turn_id.clone().unwrap();
    store
        .database
        .call(move |connection| {
            connection.execute("UPDATE turn SET state='completed' WHERE id=?1", [old_turn])?;
            Ok(())
        })
        .await
        .unwrap();
    let (next, _) = send(&store, &thread.thread_id, "next turn").await;
    assert_eq!(next.assignment, Assignment::NewTurn);
    assert_eq!(next.run_id, first.run_id);
    assert_ne!(next.turn_id, first.turn_id);
    let old: MessageDetails = serde_json::from_value(
        execute(
            &store,
            Command::MessageGet(MessageTarget {
                message_id: first.message_id.clone(),
            }),
        )
        .await
        .result,
    )
    .unwrap();
    assert_eq!(old.turn_id, first.turn_id);
    let stop = execute(
        &store,
        Command::RunStop(RunTarget {
            run_id: first.run_id.clone().unwrap(),
        }),
    )
    .await;
    assert_eq!(stop.stop_runs, vec![first.run_id.clone().unwrap()]);
    let (held, effects) = send(&store, &thread.thread_id, "during stop").await;
    assert_eq!(held.delivery_status, DeliveryStatus::Held);
    assert_eq!(held.assignment, Assignment::Pending);
    assert_eq!(held.run_id, None);
    assert_eq!(held.turn_id, None);
    assert!(effects.wake_threads.is_empty());
    let delete = request(Command::ThreadDelete(ThreadDeleteArgs {
        preview_marker: None,
        thread_id: thread.thread_id.clone(),
        descendants: false,
    }));
    assert_eq!(
        store.execute(delete, None).await.unwrap_err().code,
        ErrorCode::InvalidState
    );
    let run = first.run_id.unwrap();
    store
        .database
        .call(move |connection| {
            connection.execute(
                "UPDATE turn SET state='stopped' WHERE run_id=?1 AND state<>'completed'",
                [&run],
            )?;
            connection.execute("UPDATE run SET state='stopped' WHERE id=?1", [run])?;
            Ok(())
        })
        .await
        .unwrap();
    // Producer contract (#171): a committed deletion marks its maintenance kinds.
    store.database.signals().take();
    execute(
        &store,
        Command::ThreadDelete(ThreadDeleteArgs {
            preview_marker: None,
            thread_id: thread.thread_id.clone(),
            descendants: false,
        }),
    )
    .await;
    let marked = store.database.signals().take();
    for work in shellboy::maintenance::Work::DELETION.into_iter().chain([
        shellboy::maintenance::Work::Managed,
        shellboy::maintenance::Work::Drafts,
    ]) {
        assert!(marked.contains(work), "{work:?} not signalled by deletion");
    }
    let detail: MessageDetails = serde_json::from_value(
        execute(
            &store,
            Command::MessageGet(MessageTarget {
                message_id: held.message_id,
            }),
        )
        .await
        .result,
    )
    .unwrap();
    assert_eq!(detail.delivery_status, DeliveryStatus::Cancelled);
    let sending = request(Command::MessageSend(MessageSendArgs {
        to_human: false,
        human: false,
        resume_recovery: false,
        expect_reply: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: Some(thread.thread_id),
        text: "too late".into(),
    }));
    assert_eq!(
        store.execute(sending, None).await.unwrap_err().code,
        ErrorCode::ThreadNotFound
    );
    store
        .database
        .call(|connection| {
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM node", [], |row| row.get::<_, i64>(0))?,
                4,
                "delete leaves DAG bodies for reference-aware GC"
            );
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM gc_job WHERE state='pending'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn machine_message_does_not_resume_recovery_but_new_human_instruction_does() {
    let (_directory, store) = store().await;
    let parent = create(&store, None).await;
    let (parent_message, _) = send(&store, &parent.thread_id, "parent").await;
    let child = create(&store, Some(parent.thread_id.clone())).await;
    let origin = caller(
        &parent.thread_id,
        parent_message.run_id.as_deref().unwrap(),
        parent_message.turn_id.as_deref().unwrap(),
    );
    let accepted: MessageAccepted = serde_json::from_value(
        store
            .execute(
                request(Command::MessageSend(MessageSendArgs {
                    human: false,
                    resume_recovery: false,
                    to_human: false,
                    expect_reply: false,
                    sender_label: None,
                    deliver: Deliver::Next,
                    thread_id: Some(child.thread_id.clone()),
                    text: "child".into(),
                })),
                Some(origin.clone()),
            )
            .await
            .unwrap()
            .result,
    )
    .unwrap();
    let run = accepted.run_id.clone().unwrap();
    store.database.call(move |connection| {connection.execute("UPDATE turn SET state='recovery_wait',recovery_reason_json='{\"reason\":\"fixture\"}' WHERE run_id=?1",[run])?;Ok(())}).await.unwrap();
    let mechanical = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: false,
                human: false,
                resume_recovery: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(child.thread_id.clone()),
                text: "ordinary parent note".into(),
            })),
            Some(origin),
        )
        .await
        .unwrap();
    let mechanical_message: MessageAccepted = serde_json::from_value(mechanical.result).unwrap();
    assert_eq!(mechanical_message.delivery_status, DeliveryStatus::Held);
    assert!(
        mechanical.wake_threads.is_empty() && mechanical.quiet_wake_threads.is_empty(),
        "ordinary machine input cannot resume a recovery Turn"
    );
    let human_result = execute(
        &store,
        Command::MessageSend(MessageSendArgs {
            human: true,
            resume_recovery: false,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(child.thread_id.clone()),
            text: "human resume instruction".into(),
        }),
    )
    .await;
    let human: MessageAccepted = serde_json::from_value(human_result.result.clone()).unwrap();
    let effects = human_result;
    assert_eq!(human.turn_id, accepted.turn_id);
    assert_eq!(human.delivery_status, DeliveryStatus::Accepted);
    assert_eq!(effects.quiet_wake_threads, vec![child.thread_id.clone()]);
    assert!(effects.wake_threads.is_empty());
    let run = human.run_id.unwrap();
    store
        .database
        .call(move |connection| {
            let state: String =
                connection.query_row("SELECT state FROM turn WHERE run_id=?1", [run], |row| {
                    row.get(0)
                })?;
            assert_eq!(state, "preparing");
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopping_one_run_does_not_freeze_organizational_children() {
    let (_directory, store) = store().await;
    let parent = create(&store, None).await;
    let (parent_message, _) = send(&store, &parent.thread_id, "parent").await;
    let child = create(&store, Some(parent.thread_id.clone())).await;
    let origin = caller(
        &parent.thread_id,
        parent_message.run_id.as_deref().unwrap(),
        parent_message.turn_id.as_deref().unwrap(),
    );
    let child_result = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: false,
                human: false,
                resume_recovery: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(child.thread_id.clone()),
                text: "child".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap();
    let child_message: MessageAccepted = serde_json::from_value(child_result.result).unwrap();
    let stop = execute(
        &store,
        Command::RunStop(RunTarget {
            run_id: parent_message.run_id.clone().unwrap(),
        }),
    )
    .await;
    assert_eq!(stop.stop_runs, vec![parent_message.run_id.clone().unwrap()]);
    let child_run = child_message.run_id.unwrap();
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT state FROM run WHERE id=?1", [child_run], |r| r
                    .get::<_, String>(
                    0
                ))?,
                "live"
            );
            Ok(())
        })
        .await
        .unwrap();
    let new_child = create(&store, Some(parent.thread_id)).await;
    let result = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: false,
                human: false,
                resume_recovery: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(new_child.thread_id),
                text: "independent child admission".into(),
            })),
            Some(origin),
        )
        .await;
    assert_eq!(result.unwrap().result["assignment"], "new_run");
    store
        .database
        .call(|connection| {
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM run WHERE state='stopping'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipt_failure_rolls_back_message_run_turn_and_caches_error_without_effects() {
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    store.database.call(|connection| {connection.execute_batch("CREATE TRIGGER fixture_reject_receipt BEFORE INSERT ON receipt BEGIN SELECT RAISE(ABORT,'fixture save failure'); END;")?;Ok(())}).await.unwrap();
    let req = request(Command::MessageSend(MessageSendArgs {
        to_human: false,
        human: false,
        resume_recovery: false,
        expect_reply: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: Some(thread.thread_id),
        text: "must survive in UI".into(),
    }));
    assert_eq!(
        store.execute(req.clone(), None).await.unwrap_err().code,
        ErrorCode::SaveFailed
    );
    store
        .database
        .call(|connection| {
            for table in ["node", "run", "turn", "receipt"] {
                let sql = format!("SELECT count(*) FROM {table}");
                assert_eq!(
                    connection.query_row(&sql, [], |row| row.get::<_, i64>(0))?,
                    0
                );
            }
            connection.execute_batch("DROP TRIGGER fixture_reject_receipt")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        store.execute(req, None).await.unwrap_err().code,
        ErrorCode::SaveFailed,
        "same operation reports its saved failure; a deliberate retry uses a new operation ID"
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn list_watermark_and_query_cursor_are_bounded_and_filter_bound() {
    let (_directory, store) = store().await;
    for _ in 0..3 {
        create(&store, None).await;
    }
    let first = execute(
        &store,
        Command::ThreadList(ThreadListArgs {
            limit: 1,
            cursor: None,
            query: None,
            awaiting: None,
            archived: Default::default(),
            tags: Vec::new(),
            cwd: None,
        }),
    )
    .await;
    let first: Page<Value> = serde_json::from_value(first.result).unwrap();
    assert_eq!(first.items.len(), 1);
    create(&store, None).await;
    let second = execute(
        &store,
        Command::ThreadList(ThreadListArgs {
            limit: 200,
            cursor: first.next_cursor.clone(),
            query: None,
            awaiting: None,
            archived: Default::default(),
            tags: Vec::new(),
            cwd: None,
        }),
    )
    .await;
    let second: Page<Value> = serde_json::from_value(second.result).unwrap();
    assert_eq!(
        second.items.len(),
        2,
        "new Thread stays beyond first-page watermark"
    );
    let wrong = store
        .execute(
            request(Command::ThreadList(ThreadListArgs {
                limit: 1,
                cursor: first.next_cursor,
                query: Some("changed".into()),
                awaiting: None,
                archived: Default::default(),
                tags: Vec::new(),
                cwd: None,
            })),
            None,
        )
        .await;
    assert_eq!(wrong.unwrap_err().code, ErrorCode::CursorExpired);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_list_awaiting_human_pages_only_human_waits_with_oldest_timestamp() {
    let (_directory, store) = store().await;
    let human = create(&store, None).await;
    let other = create(&store, None).await;
    let (origin_message, _) = send(&store, &human.thread_id, "model turn").await;
    let origin = caller(
        &human.thread_id,
        origin_message.run_id.as_deref().unwrap(),
        origin_message.turn_id.as_deref().unwrap(),
    );
    let human_receipt = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: true,
                human: false,
                resume_recovery: false,
                expect_reply: true,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: None,
                text: "first question".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap();
    let first_id = serde_json::from_value::<MessageAccepted>(human_receipt.result)
        .unwrap()
        .message_id;
    let second_receipt = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: true,
                human: false,
                resume_recovery: false,
                expect_reply: true,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: None,
                text: "second question".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap();
    let second_id = serde_json::from_value::<MessageAccepted>(second_receipt.result)
        .unwrap()
        .message_id;
    store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: false,
                human: false,
                resume_recovery: false,
                expect_reply: true,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(other.thread_id.clone()),
                text: "thread duty".into(),
            })),
            Some(origin),
        )
        .await
        .unwrap();
    let first = execute(
        &store,
        Command::ThreadList(ThreadListArgs {
            limit: 1,
            cursor: None,
            query: None,
            awaiting: Some("human".into()),
            archived: Default::default(),
            tags: vec![],
            cwd: None,
        }),
    )
    .await;
    let first: Page<ThreadSummary> = serde_json::from_value(first.result).unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].thread_id, human.thread_id);
    assert_eq!(first.items[0].awaiting_human_count, 2);
    let oldest: String = store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT min(created_at) FROM receipt WHERE message_node_id IN (?1,?2)",
                [first_id, second_id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        first.items[0].awaiting_human_oldest_at.as_deref(),
        Some(oldest.as_str())
    );
    assert!(first.next_cursor.is_none());
    let all = execute(
        &store,
        Command::ThreadList(ThreadListArgs {
            limit: 20,
            cursor: None,
            query: None,
            awaiting: Some("human".into()),
            archived: Default::default(),
            tags: vec![],
            cwd: None,
        }),
    )
    .await;
    let all: Page<ThreadSummary> = serde_json::from_value(all.result).unwrap();
    assert_eq!(
        all.items
            .iter()
            .map(|i| i.thread_id.as_str())
            .collect::<Vec<_>>(),
        vec![human.thread_id.as_str()]
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_new_human_send_assigns_only_unassigned_stopping_messages_once() {
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let (old, _) = send(&store, &thread.thread_id, "old assigned input").await;
    execute(
        &store,
        Command::RunStop(RunTarget {
            run_id: old.run_id.clone().unwrap(),
        }),
    )
    .await;
    let (pending, _) = send(&store, &thread.thread_id, "saved while stopping").await;
    assert_eq!(pending.assignment, Assignment::Pending);
    let old_run = old.run_id.clone().unwrap();
    store
        .database
        .call(move |connection| {
            connection.execute(
                "UPDATE turn SET state='stopped' WHERE run_id=?1",
                [&old_run],
            )?;
            connection.execute("UPDATE run SET state='stopped' WHERE id=?1", [old_run])?;
            Ok(())
        })
        .await
        .unwrap();
    let restart_request = request(Command::MessageSend(MessageSendArgs {
        to_human: false,
        human: false,
        resume_recovery: false,
        expect_reply: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: Some(thread.thread_id.clone()),
        text: "explicit human restart".into(),
    }));
    let restart_result = store.execute(restart_request.clone(), None).await.unwrap();
    let restart: MessageAccepted = serde_json::from_value(restart_result.result.clone()).unwrap();
    assert_eq!(restart.assignment, Assignment::NewRun);
    let resumed: MessageDetails = serde_json::from_value(
        execute(
            &store,
            Command::MessageGet(MessageTarget {
                message_id: pending.message_id.clone(),
            }),
        )
        .await
        .result,
    )
    .unwrap();
    assert_eq!(resumed.run_id, restart.run_id);
    assert_eq!(resumed.turn_id, restart.turn_id);
    assert_eq!(
        resumed.delivery_status,
        DeliveryStatus::Applied,
        "explicit recovery already adopted the never-assigned input into history"
    );
    let original: MessageDetails = serde_json::from_value(
        execute(
            &store,
            Command::MessageGet(MessageTarget {
                message_id: old.message_id.clone(),
            }),
        )
        .await
        .result,
    )
    .unwrap();
    assert_eq!(
        original.run_id, old.run_id,
        "already assigned input cannot move to the resumed Run"
    );
    assert_eq!(original.turn_id, old.turn_id);
    assert_eq!(
        original.delivery_status,
        DeliveryStatus::Applied,
        "assigned-but-unadmitted input is recovered without reassignment"
    );
    let replay = store.execute(restart_request, None).await.unwrap();
    assert_eq!(replay.result, restart_result.result);
    assert!(
        replay.wake_threads.is_empty(),
        "saved acceptance cannot repeat recovery effects"
    );
    let expected = vec![old.message_id.clone(), pending.message_id.clone()];
    let new_message = restart.message_id.clone();
    let target = thread.thread_id.clone();
    store.database.call(move |db| {
        let nodes: Vec<String> = db.prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
            .query_map([&target], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        assert_eq!(nodes, expected, "original receipt order precedes the new unadmitted send");
        for id in &nodes {
            assert_eq!(db.query_row("SELECT state,path_admission,hold_reason FROM receipt WHERE message_node_id=?1", [id], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?,
                ("applied".into(),"admitted".into(),"recovered_at_explicit_resume".into()));
        }
        assert_eq!(db.query_row("SELECT path_admission FROM receipt WHERE message_node_id=?1", [new_message], |r|r.get::<_,String>(0))?, "unadmitted");
        assert_eq!(db.query_row("SELECT count(*) FROM receipt WHERE target_thread_id=?1", [&target], |r|r.get::<_,i64>(0))?, 3);
        assert_eq!(db.query_row("SELECT count(*) FROM event WHERE target_thread_id=?1 AND json_extract(payload_ref_json,'$.kind')='interrupted_messages_recovered'", [target], |r|r.get::<_,i64>(0))?, 1);
        Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn first_send_creates_thread_and_message_atomically_and_retransmits_once() {
    let (_directory, store) = store().await;
    let args = ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    };
    store.database.call(|db|{db.execute_batch("CREATE TRIGGER fixture_initial_save_failure BEFORE INSERT ON receipt BEGIN SELECT RAISE(ABORT,'fixture first-send failure'); END;")?;Ok(())}).await.unwrap();
    assert_eq!(
        store
            .send_new_thread(
                "instance:first-failed".into(),
                args.clone(),
                "keep this draft".into()
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SaveFailed
    );
    store
        .database
        .call(|db| {
            for table in ["thread", "run", "turn", "node", "receipt"] {
                assert_eq!(
                    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))?,
                    0,
                    "initial failure leaves no orphan {table}"
                );
            }
            db.execute_batch("DROP TRIGGER fixture_initial_save_failure")?;
            Ok(())
        })
        .await
        .unwrap();
    let first = store
        .send_new_thread(
            "instance:first-success".into(),
            args.clone(),
            "accepted once".into(),
        )
        .await
        .unwrap();
    let again = store
        .send_new_thread(
            "instance:first-success".into(),
            args,
            "accepted once".into(),
        )
        .await
        .unwrap();
    assert_eq!(first.result, again.result);
    assert!(again.wake_threads.is_empty());
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM receipt", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_intent_is_reserved_once_and_unknown_pending_result_is_not_reexecuted() {
    use shellboy::control::db::RuntimeReservation;
    let (_directory, store) = store().await;
    let request = request(Command::ShellWrite(ShellWriteArgs {
        execution_id: "fixture".into(),
        text: "intent".into(),
        encoding: "UTF-8".into(),
    }));
    assert!(matches!(
        store.reserve_runtime(request.clone(), None).await.unwrap(),
        RuntimeReservation::New
    ));
    assert!(matches!(
        store.reserve_runtime(request.clone(), None).await.unwrap(),
        RuntimeReservation::Pending
    ));
    let mut conflict = request.clone();
    if let Command::ShellWrite(args) = &mut conflict.command {
        args.text = "different".into();
    }
    assert_eq!(
        store
            .reserve_runtime(conflict, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    store
        .finalize_runtime(
            request.operation_id.clone(),
            Ok(serde_json::json!({"bytes_written":6})),
        )
        .await
        .unwrap();
    let RuntimeReservation::Saved(Ok(result)) =
        store.reserve_runtime(request.clone(), None).await.unwrap()
    else {
        panic!("saved result required");
    };
    assert_eq!(result["bytes_written"], 6);
    assert_eq!(
        store
            .finalize_runtime(request.operation_id, Ok(result))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidState
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_effort_clear_is_null_and_model_change_retains_an_existing_effort() {
    let (_root, store) = store().await;
    let thread = create(&store, None).await;
    let changed = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            auto_compact_target_percent: None,
            summary_max_chars: None,
            max_tokens: None,
            spill: None,
            auto_compact_at: None,
            agents: None,
            title: None,
            thread_id: Some(thread.thread_id.clone()),
            connection: None,
            model: None,
            effort: Some("high".into()),
            cwd: None,
            parent_id: None,
        })),
    )
    .await;
    assert_eq!(changed.result["effort"], "high");
    let changed = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            auto_compact_target_percent: None,
            summary_max_chars: None,
            max_tokens: None,
            spill: None,
            auto_compact_at: None,
            agents: None,
            title: None,
            thread_id: Some(thread.thread_id.clone()),
            connection: None,
            model: Some("next-model".into()),
            effort: None,
            cwd: None,
            parent_id: None,
        })),
    )
    .await;
    assert_eq!(changed.result["effort"], "high");
    let cleared = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            auto_compact_target_percent: None,
            summary_max_chars: None,
            max_tokens: None,
            spill: None,
            auto_compact_at: None,
            agents: None,
            title: None,
            thread_id: Some(thread.thread_id),
            connection: None,
            model: None,
            effort: Some(String::new()),
            cwd: None,
            parent_id: None,
        })),
    )
    .await;
    let details: ThreadDetails = serde_json::from_value(cleared.result).unwrap();
    assert_eq!(details.effort, None);
    assert!(
        shellboy::model::response_request(&details.model, vec![], details.effort.as_deref(), true)
            .get("reasoning")
            .is_none()
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn any_caller_may_stop_any_independent_run_without_cascade() {
    let (_directory, store) = store().await;
    let parent = create(&store, None).await;
    let (parent_message, _) = send(&store, &parent.thread_id, "parent").await;
    let origin = caller(
        &parent.thread_id,
        parent_message.run_id.as_deref().unwrap(),
        parent_message.turn_id.as_deref().unwrap(),
    );
    let independent = create(&store, None).await;
    let (independent_message, _) = send(&store, &independent.thread_id, "independent").await;
    let historical_child = create(&store, Some(parent.thread_id.clone())).await;
    let (unowned_message, _) = send(
        &store,
        &historical_child.thread_id,
        "human-started child is independent",
    )
    .await;
    // No authority check (#256): a non-owner Run may stop an unowned Run.
    let non_owner_stop = request(Command::RunStop(RunTarget {
        run_id: unowned_message.run_id.clone().unwrap(),
    }));
    let operation_id = non_owner_stop.operation_id.clone();
    let stopped_unowned = store
        .execute(non_owner_stop, Some(origin.clone()))
        .await
        .unwrap();
    assert_eq!(
        stopped_unowned.stop_runs,
        vec![unowned_message.run_id.clone().unwrap()]
    );
    // The operation receipt records who stopped it.
    let recorded_run = parent_message.run_id.clone().unwrap();
    store
        .database
        .call(move |db| {
            let origin: String = db.query_row(
                "SELECT origin_json FROM operation_receipt WHERE operation_id=?1",
                [operation_id],
                |r| r.get(0),
            )?;
            assert!(origin.contains(&recorded_run), "{origin}");
            Ok(())
        })
        .await
        .unwrap();
    let owned = create(&store, Some(parent.thread_id.clone())).await;
    let accepted = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: false,
                human: false,
                resume_recovery: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(owned.thread_id),
                text: "owned child".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap();
    let child: MessageAccepted = serde_json::from_value(accepted.result).unwrap();
    let stopped = store
        .execute(
            request(Command::RunStop(RunTarget {
                run_id: parent_message.run_id.clone().unwrap(),
            })),
            Some(origin),
        )
        .await
        .unwrap();
    assert_eq!(stopped.stop_runs, vec![parent_message.run_id.unwrap()]);
    let child_run = child.run_id.unwrap();
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT state FROM run WHERE id=?1", [child_run], |r| r
                    .get::<_, String>(
                    0
                ))?,
                "live"
            );
            Ok(())
        })
        .await
        .unwrap();
    let human = execute(
        &store,
        Command::RunStop(RunTarget {
            run_id: independent_message.run_id.unwrap(),
        }),
    )
    .await;
    assert_eq!(human.stop_runs.len(), 1);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn authenticated_origin_credentials_are_not_saved_in_receipts_or_runtime_intents() {
    use shellboy::control::db::RuntimeReservation;
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let (started, _) = send(&store, &thread.thread_id, "start").await;
    let origin = caller(
        &thread.thread_id,
        started.run_id.as_deref().unwrap(),
        started.turn_id.as_deref().unwrap(),
    );
    let req = request(Command::MessageSend(MessageSendArgs {
        to_human: false,
        human: false,
        resume_recovery: false,
        expect_reply: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: None,
        text: "from authenticated shell".into(),
    }));
    let first = store
        .execute(req.clone(), Some(origin.clone()))
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(&first.result)
            .unwrap()
            .contains(&origin.token)
    );
    assert_eq!(
        store
            .execute(req.clone(), Some(origin.clone()))
            .await
            .unwrap()
            .result,
        first.result
    );
    let mut different = origin.clone();
    different.token = "different-issued-credential".into();
    assert_eq!(
        store
            .execute(req.clone(), Some(different))
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict,
        "fingerprints remain credential-bound"
    );
    let runtime = request(Command::ShellWrite(ShellWriteArgs {
        execution_id: origin.execution_id.clone(),
        text: "intent".into(),
        encoding: "UTF-8".into(),
    }));
    assert!(matches!(
        store
            .reserve_runtime(runtime.clone(), Some(origin.clone()))
            .await
            .unwrap(),
        RuntimeReservation::New
    ));
    assert!(matches!(
        store
            .reserve_runtime(runtime, Some(origin.clone()))
            .await
            .unwrap(),
        RuntimeReservation::Pending
    ));
    let expected = serde_json::to_value(origin.identity()).unwrap();
    store.database.call(move |db| {
        for sql in ["SELECT origin_json FROM operation_receipt WHERE json_extract(origin_json,'$.execution_id') IS NOT NULL", "SELECT sender_json FROM receipt WHERE json_extract(sender_json,'$.execution_id') IS NOT NULL"] {
            let bodies = db.prepare(sql)?.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert!(!bodies.is_empty());
            for body in bodies {
                assert!(!body.contains("fixture-token"));
                let mut expected_record = expected.clone();
                if sql.contains("SELECT sender_json") { expected_record["kind"] = serde_json::json!("model"); }
                assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), expected_record);
            }
        }
        Ok(())
    }).await.unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn retry_schedules_exactly_one_quiet_driver_wake() {
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let (accepted, _) = send(&store, &thread.thread_id, "retry fixture").await;
    let run = accepted.run_id.unwrap();
    let fixture_run = run.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE turn SET state='recovery_wait' WHERE run_id=?1",
                [fixture_run],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let effects = execute(&store, Command::RunRetry(RunTarget { run_id: run })).await;
    assert_eq!(
        effects.wake_threads.len() + effects.quiet_wake_threads.len(),
        1,
        "retry must not kick twice"
    );
    assert!(
        effects.wake_threads.is_empty(),
        "retry is not new human input"
    );
    assert_eq!(effects.quiet_wake_threads, vec![thread.thread_id]);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipt_saturation_rejects_before_effects_but_keeps_replay_observation_and_maintenance() {
    use shellboy::control::db::RuntimeReservation;
    let (_directory, store) = store().await;
    let create_request = request(Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }));
    let _created = store
        .execute(create_request.clone(), None)
        .await
        .unwrap()
        .result;
    let intent = request(Command::ShellWrite(ShellWriteArgs {
        execution_id: "fixture".into(),
        text: "never written".into(),
        encoding: "UTF-8".into(),
    }));
    assert!(matches!(
        store.reserve_runtime(intent.clone(), None).await.unwrap(),
        RuntimeReservation::New
    ));
    let id = intent.operation_id.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE operation_receipt SET reserved_bytes=268435456 WHERE operation_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let new_request = request(Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }));
    for _ in 0..3 {
        assert_eq!(
            store
                .execute(new_request.clone(), None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::StorageLimit
        );
        assert_eq!(
            store
                .execute(create_request.clone(), None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::OperationExpired,
            "pressure may reclaim the terminal result but must not recreate its Thread"
        );
        assert!(matches!(
            store.reserve_runtime(intent.clone(), None).await.unwrap(),
            RuntimeReservation::Pending
        ));
    }
    let other = request(Command::ShellWrite(ShellWriteArgs {
        execution_id: "fixture".into(),
        text: "never written either".into(),
        encoding: "UTF-8".into(),
    }));
    assert_eq!(
        store.reserve_runtime(other, None).await.unwrap_err().code,
        ErrorCode::StorageLimit
    );
    let status = store
        .execute(
            request(Command::OperationGet(OperationTarget {
                operation_id: intent.operation_id.clone(),
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(status["state"], "pending");
    assert_eq!(status["result_uncertain"], true);
    assert_eq!(
        store
            .execute(request(Command::Status(StatusArgs::default())), None)
            .await
            .unwrap()
            .result["receipt_budget"]["at_limit"],
        true
    );
    assert_eq!(store.collect_receipts().await.unwrap().collected, 0);
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM operation_receipt", [], |r| r
                    .get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |r| r
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
async fn old_terminal_receipt_collection_preserves_pending_and_expires_old_execution_ids() {
    let (_directory, store) = store().await;
    let create_request = request(Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }));
    let outcome = store
        .execute(create_request.clone(), None)
        .await
        .unwrap()
        .result;
    store.database.call(|db|{
        db.execute("UPDATE operation_receipt SET created_at='2000-01-01'",[])?;
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at)VALUES('old:uncertain','old','null','hash','pending','2000-01-01')",[])?;
        db.execute("INSERT INTO app_meta(key,value)SELECT 'compact.result.'||operation_id,'{}' FROM operation_receipt WHERE state='completed'",[])?;
        Ok(())
    }).await.unwrap();
    let store = Store::new(
        store.database.clone(),
        store.config.clone(),
        store.startup_cwd.clone(),
        "next".into(),
    );
    assert_eq!(store.collect_receipts().await.unwrap().collected, 1);
    assert_eq!(
        store.execute(create_request, None).await.unwrap_err().code,
        ErrorCode::OperationExpired
    );
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id='old:uncertain'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key LIKE 'compact.result.%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row("SELECT id FROM thread", [], |r| r.get::<_, String>(0))?,
                outcome["thread_id"].as_str().unwrap()
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn old_receipt_and_compaction_companion_collection_roll_back_together_on_failure() {
    let (_directory, store) = store().await;
    store.database.call(|db|{
        db.execute_batch("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json)VALUES('old:receipt','old','null','hash','completed','2000-01-01','{}');INSERT INTO app_meta VALUES('compact.result.old:receipt','{}');CREATE TRIGGER fail_receipt_collection BEFORE DELETE ON operation_receipt BEGIN SELECT RAISE(ABORT,'collection failure fixture');END;")?;Ok(())
    }).await.unwrap();
    assert_eq!(
        store.collect_receipts().await.unwrap_err().code,
        ErrorCode::SaveFailed
    );
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id='old:receipt'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key='compact.result.old:receipt'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            db.execute_batch("DROP TRIGGER fail_receipt_collection")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(store.collect_receipts().await.unwrap().collected, 1);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn quota_refusal_follows_complete_bounded_reclamation_without_accepting_new_work() {
    let (_directory, store) = store().await;
    store.database.call(|db|{
        let tx=db.transaction()?;
        tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes)VALUES('instance:full','instance','null','hash','pending','2000-01-01',268435456)",[])?;
        for n in 0..300 {tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,result_ref_json)VALUES(?1,'old','null','hash','completed','2000-01-01','{}')",[format!("old:{n}")])?;}
        tx.commit()?;Ok(())
    }).await.unwrap();
    let create_request = request(Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }));
    let write = request(Command::ShellWrite(ShellWriteArgs {
        execution_id: "never-spawned".into(),
        text: "no side effect".into(),
        encoding: "UTF-8".into(),
    }));
    // Each unit remains bounded, but the first refusal must wait for the
    // entire reclaimable set rather than returning after one unit.
    for n in 0..3 {
        let error = if n == 1 {
            store
                .reserve_runtime(write.clone(), None)
                .await
                .unwrap_err()
        } else {
            store
                .execute(create_request.clone(), None)
                .await
                .unwrap_err()
        };
        assert_eq!(error.code, ErrorCode::StorageLimit);
        store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM operation_receipt WHERE instance_id='old'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    db.query_row(
                        "SELECT count(*) FROM operation_receipt WHERE instance_id='instance'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    1
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM thread", [], |r| r.get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_filter_validation_and_valid_reads_never_create_receipts() {
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let before = store
        .database
        .call(|db| {
            Ok((
                db.total_changes(),
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            ))
        })
        .await
        .unwrap();
    for (since, state) in [("invalid", None), ("2026-10-25T07:00:00Z", Some("unknown"))] {
        let args = HistoryListArgs {
            excerpt_chars: shellboy::protocol::DEFAULT_HISTORY_EXCERPT_CHARS,
            thread_id: thread.thread_id.clone(),
            limit: DEFAULT_LIST_LIMIT,
            cursor: None,
            run_id: None,
            since: Some(since.into()),
            state: state.map(str::to_owned),
        };
        assert_eq!(
            store
                .execute(request(Command::HistoryList(args)), None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    let after = store
        .database
        .call(|db| {
            Ok((
                db.total_changes(),
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "invalid filters must not reserve/save a failed read receipt"
    );
    let valid = request(Command::HistoryList(HistoryListArgs {
        excerpt_chars: shellboy::protocol::DEFAULT_HISTORY_EXCERPT_CHARS,
        thread_id: thread.thread_id,
        limit: DEFAULT_LIST_LIMIT,
        cursor: None,
        run_id: None,
        since: Some("2026-10-25T16:00:00+09:00".into()),
        state: Some("running".into()),
    }));
    let original = store.execute(valid.clone(), None).await.unwrap().result;
    assert_eq!(store.execute(valid, None).await.unwrap().result, original);
    let receipts = store
        .database
        .call(|db| {
            Ok(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(receipts, before.1);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn normal_receipt_saturation_preserves_bounded_run_and_shell_stop_admission_and_replay() {
    use shellboy::control::db::RuntimeReservation;
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let (message, _) = send(&store, &thread.thread_id, "work to stop").await;
    let run_id = message.run_id.unwrap();
    let pressure_id = "instance:receipt-pressure".to_owned();
    assert!(matches!(
        store
            .reserve_internal(
                pressure_id.clone(),
                serde_json::json!({"fixture":"pressure"})
            )
            .await
            .unwrap(),
        RuntimeReservation::New
    ));
    // Real pending reservations and trigger-maintained counters, without a
    // hundreds-of-MiB saved outcome. The chosen row owns only the missing bytes.
    async fn fill(store: &Store, pressure: String, target: u64) {
        store.database.call(move |db| {
            let actual: i64 = db.query_row(
                "SELECT logical_bytes FROM quota_counter WHERE category='operation_receipt'",
                [], |row| row.get(0),
            )?;
            let other: i64 = db.query_row(
                "SELECT COALESCE(SUM(reserved_bytes),0) FROM operation_receipt WHERE instance_id='instance' AND state='pending' AND operation_id<>?1",
                [&pressure], |row| row.get(0),
            )?;
            assert!(target as i64 >= actual + other);
            db.execute(
                "UPDATE operation_receipt SET reserved_bytes=?2 WHERE operation_id=?1 AND state='pending'",
                rusqlite::params![pressure, target as i64 - actual - other],
            )?;
            Ok(())
        }).await.unwrap();
    }
    fill(&store, pressure_id.clone(), 256 * 1024 * 1024).await;
    let status = execute(&store, Command::Status(StatusArgs::default()))
        .await
        .result;
    assert_eq!(status["receipt_budget"]["available_bytes"], 0);
    assert_eq!(
        status["receipt_budget"]["control_headroom_bytes"],
        33_562_624
    );
    assert_eq!(status["receipt_budget"]["control_limit_bytes"], 301_998_080);
    assert_eq!(
        status["receipt_budget"]["control_available_bytes"],
        33_562_624
    );
    let normal = request(Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }));
    assert_eq!(
        store.execute(normal, None).await.unwrap_err().code,
        ErrorCode::StorageLimit
    );
    assert_eq!(
        store
            .reserve_runtime(
                request(Command::ShellWrite(ShellWriteArgs {
                    execution_id: "not-started".into(),
                    text: "not written".into(),
                    encoding: "UTF-8".into(),
                })),
                None
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageLimit
    );

    // ShellStop retains the runtime intent contract while consuming the stop
    // allowance. A concurrent same-ID observer cannot execute it twice.
    let shell_stop = request(Command::ShellStop(ExecutionTarget {
        execution_id: "fixture-shell".into(),
    }));
    assert!(matches!(
        store
            .reserve_runtime(shell_stop.clone(), None)
            .await
            .unwrap(),
        RuntimeReservation::New
    ));
    assert!(matches!(
        store
            .reserve_runtime(shell_stop.clone(), None)
            .await
            .unwrap(),
        RuntimeReservation::Pending
    ));
    let mut shell_conflict = shell_stop.clone();
    shell_conflict.command = Command::ShellStop(ExecutionTarget {
        execution_id: "another-shell".into(),
    });
    assert_eq!(
        store
            .reserve_runtime(shell_conflict.clone(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );

    // RunStop takes the DB path, changes state and generation once, and saves
    // its result in the same transaction while the shell reservation is live.
    let run_stop = request(Command::RunStop(RunTarget {
        run_id: run_id.clone(),
    }));
    let stopped = store.execute(run_stop.clone(), None).await.unwrap();
    assert_eq!(stopped.result["state"], "stopping");
    assert_eq!(stopped.stop_runs, vec![run_id.clone()]);
    let replay = store.execute(run_stop.clone(), None).await.unwrap();
    assert_eq!(replay.result, stopped.result);
    assert!(
        replay.stop_runs.is_empty(),
        "saved outcomes do not repeat stop effects"
    );
    let mut run_conflict = run_stop.clone();
    run_conflict.command = Command::RunStop(RunTarget {
        run_id: "another-run".into(),
    });
    assert_eq!(
        store
            .execute(run_conflict.clone(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    let saved_shell = serde_json::json!({"accepted":true});
    store
        .finalize_runtime(shell_stop.operation_id.clone(), Ok(saved_shell.clone()))
        .await
        .unwrap();

    // Stop traffic itself is finite. At that boundary fresh stop IDs are
    // refused. Pressure can expire terminal results; their retained identities
    // still prevent replaying effects and reject different request arguments.
    fill(&store, pressure_id, 301_998_080).await;
    assert_eq!(
        store
            .execute(
                request(Command::RunStop(RunTarget {
                    run_id: run_id.clone()
                })),
                None
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageLimit
    );
    assert_eq!(
        store
            .reserve_runtime(
                request(Command::ShellStop(ExecutionTarget {
                    execution_id: "fresh-stop".into()
                })),
                None
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageLimit
    );
    assert_eq!(
        store.execute(run_stop, None).await.unwrap_err().code,
        ErrorCode::OperationExpired
    );
    assert!(matches!(
        store.reserve_runtime(shell_stop, None).await.unwrap(),
        RuntimeReservation::Saved(Err(ApiError {
            code: ErrorCode::OperationExpired,
            ..
        }))
    ));
    assert_eq!(
        store.execute(run_conflict, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    assert_eq!(
        store
            .reserve_runtime(shell_conflict, None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationConflict
    );
    let status = execute(&store, Command::Status(StatusArgs::default()))
        .await
        .result;
    assert!(
        status["receipt_budget"]["control_available_bytes"]
            .as_u64()
            .unwrap()
            < 8 * 1024 * 1024,
        "reclaiming small terminal results does not admit a full reservation"
    );
    assert_eq!(status["receipt_budget"]["available_bytes"], 0);
    assert_eq!(
        store
            .execute(request(Command::Status(StatusArgs::default())), None)
            .await
            .unwrap()
            .result["receipt_budget"]["at_limit"],
        true
    );
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM thread", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM shell_execution", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT generation FROM turn WHERE run_id=?1",
                    [&run_id],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_recovery_releases_only_the_resumed_turn_and_zero_row_cas_releases_nothing() {
    for retry in [false, true] {
        for suppress in [false, true] {
            let (_directory, store) = store().await;
            let thread = create(&store, None).await;
            let (accepted, _) = send(&store, &thread.thread_id, "current instruction").await;
            let run = accepted.run_id.clone().unwrap();
            let turn = accepted.turn_id.clone().unwrap();
            let target_thread = thread.thread_id.clone();
            let target_run = run.clone();
            let target_turn = turn.clone();
            store.database.call(move |db| {
                let tx = db.transaction()?;
                tx.execute("UPDATE turn SET state='recovery_wait',generation=4,recovery_reason_json='{\"reason\":\"fixture\"}' WHERE id=?1",[&target_turn])?;
                tx.execute("UPDATE receipt SET state='held',hold_reason='recovery_wait',updated_at='keep-current' WHERE turn_id=?1",[&target_turn])?;
                // A stale/imported older receipt must not be revived by retrying
                // another Turn in the same Run. Its assignment stays immutable.
                tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('older-turn',?1,?2,'completed','before','before')",rusqlite::params![target_thread,target_run])?;
                tx.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json)VALUES('older-message','older-turn','user','before','{}'),('other-held',?1,'user','now','{}')",[&target_turn])?;
                tx.execute("INSERT INTO receipt(id,client_operation_id,sender_json,target_thread_id,message_node_id,run_id,turn_id,state,created_at,updated_at,hold_reason)VALUES('older-receipt','old-op','{}',?1,'older-message',?2,'older-turn','held','before','keep-older','recovery_wait'),('other-receipt','other-op','{}',?1,'other-held',?2,?3,'held','before','keep-other','stopping')",rusqlite::params![target_thread,target_run,target_turn])?;
                if suppress {
                    tx.execute_batch("CREATE TRIGGER suppress_recovery BEFORE UPDATE OF state ON turn WHEN OLD.state='recovery_wait' AND NEW.state='preparing' BEGIN SELECT RAISE(IGNORE); END;")?;
                }
                tx.commit()?;
                Ok(())
            }).await.unwrap();
            let command = if retry {
                Command::RunRetry(RunTarget {
                    run_id: run.clone(),
                })
            } else {
                Command::MessageSend(MessageSendArgs {
                    human: true,
                    resume_recovery: false,
                    to_human: false,
                    expect_reply: false,
                    sender_label: None,
                    deliver: Deliver::Next,
                    thread_id: Some(thread.thread_id.clone()),
                    text: "explicit human recovery".into(),
                })
            };
            let recovery_request = request(command);
            let result = store.execute(recovery_request.clone(), None).await;
            if suppress {
                assert_eq!(
                    result.unwrap_err().code,
                    if retry {
                        ErrorCode::InvalidState
                    } else {
                        ErrorCode::OperationConflict
                    }
                );
                let replay = store.execute(recovery_request, None).await.unwrap_err();
                assert_eq!(
                    replay.code,
                    if retry {
                        ErrorCode::InvalidState
                    } else {
                        ErrorCode::OperationConflict
                    },
                    "a failed zero-row CAS is replayed without trying recovery again"
                );
            } else {
                let effects = result.unwrap();
                assert!(
                    effects.wake_threads.is_empty(),
                    "RunRetry and ordinary queued human recovery both use a quiet driver wake"
                );
                assert_eq!(effects.quiet_wake_threads, vec![thread.thread_id.clone()]);
                let replay = store.execute(recovery_request, None).await.unwrap();
                assert_eq!(replay.result, effects.result);
                assert!(
                    replay.wake_threads.is_empty()
                        && replay.quiet_wake_threads.is_empty()
                        && replay.changed_threads.is_empty(),
                    "saved recovery result cannot repeat any wake or change effect"
                );
            }
            let current_message = accepted.message_id;
            store.database.call(move |db| {
                let current:(String,i64,Option<String>)=db.query_row("SELECT state,generation,recovery_reason_json FROM turn WHERE id=?1",[turn],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
                assert_eq!(current.0, if suppress {"recovery_wait"}else{"preparing"});
                assert_eq!(current.1, if suppress {4}else{5});
                assert_eq!(current.2.is_some(),suppress);
                let receipt:(String,Option<String>)=db.query_row("SELECT state,hold_reason FROM receipt WHERE message_node_id=?1",[current_message],|row|Ok((row.get(0)?,row.get(1)?)))?;
                assert_eq!(receipt,if suppress {("held".into(),Some("recovery_wait".into()))}else{("accepted".into(),None)});
                for (id,reason,updated) in [("older-receipt","recovery_wait","keep-older"),("other-receipt","stopping","keep-other")] {
                    let row:(String,String,String)=db.query_row("SELECT state,hold_reason,updated_at FROM receipt WHERE id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
                    assert_eq!(row,("held".into(),reason.into(),updated.into()));
                }
                assert_eq!(db.query_row("SELECT state FROM turn WHERE id='older-turn'",[],|row|row.get::<_,String>(0))?,"completed");
                let events:i64=db.query_row("SELECT COUNT(*) FROM event WHERE kind='context_changed' AND json_extract(payload_ref_json,'$.kind')='recovery_resumed'",[],|row|row.get(0))?;
                assert_eq!(events,i64::from(!suppress));
                assert_eq!(db.query_row("SELECT count(*) FROM receipt",[],|row|row.get::<_,i64>(0))?,3+i64::from(!retry && !suppress),
                    "zero-row CAS/replay must not accept an extra message");
                assert_eq!(db.query_row("SELECT state FROM run WHERE id=?1",[run],|row|row.get::<_,String>(0))?,"live");
                Ok(())
            }).await.unwrap();
            store.database.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn large_database_observations_do_not_grow_receipts_even_when_pending_reservations_are_full()
{
    use shellboy::control::db::RuntimeReservation;
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    let (message, _) = send(&store, &thread.thread_id, &"history-body-".repeat(6000)).await;
    let pending = request(Command::ShellWrite(ShellWriteArgs {
        execution_id: "not-executed".into(),
        text: "not-written".into(),
        encoding: "UTF-8".into(),
    }));
    assert!(matches!(
        store.reserve_runtime(pending.clone(), None).await.unwrap(),
        RuntimeReservation::New
    ));
    let pressure_id = pending.operation_id.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE operation_receipt SET reserved_bytes=268435456 WHERE operation_id=?1",
                [pressure_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = || {
        |db: &mut rusqlite::Connection| {
            Ok((
                db.total_changes(),
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row(
                    "SELECT logical_bytes FROM quota_counter WHERE category='operation_receipt'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        }
    };
    let before = store.database.call(snapshot()).await.unwrap();
    let commands = [
        Command::Status(StatusArgs::default()),
        Command::ThreadList(ThreadListArgs::default()),
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.thread_id.clone()),
        }),
        Command::ThreadDeletePreview(ThreadDeleteArgs {
            preview_marker: None,
            thread_id: thread.thread_id.clone(),
            descendants: false,
        }),
        Command::MessageGet(MessageTarget {
            message_id: message.message_id.clone(),
        }),
        Command::OperationGet(OperationTarget {
            operation_id: pending.operation_id,
        }),
        Command::RunGet(RunTarget {
            run_id: message.run_id.unwrap(),
        }),
        Command::TurnGet(TurnTarget {
            turn_id: message.turn_id.unwrap(),
        }),
        Command::HistoryList(HistoryListArgs {
            excerpt_chars: shellboy::protocol::DEFAULT_HISTORY_EXCERPT_CHARS,
            thread_id: thread.thread_id.clone(),
            limit: 50,
            cursor: None,
            run_id: None,
            since: None,
            state: None,
        }),
        Command::HistoryGet(HistoryGetArgs {
            end_message_only: false,
            parts: false,
            node_id: message.message_id.clone(),
            limit_bytes: 65536,
            cursor: None,
            encoding: "UTF-8".into(),
        }),
        Command::HistoryRelations(HistoryRelationsArgs {
            node_id: message.message_id,
            relation: HistoryRelation::Sources,
            limit: 50,
            cursor: None,
        }),
        Command::AttachmentList(ThreadTarget {
            thread_id: Some(thread.thread_id),
        }),
    ];
    for _ in 0..20 {
        for command in &commands {
            let result = store.execute(request(command.clone()), None).await;
            assert!(result.is_ok(), "{}: {result:?}", command.name());
        }
    }
    assert_eq!(
        store.database.call(snapshot()).await.unwrap(),
        before,
        "observations must neither reserve/save receipts nor collect pending work"
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn fresh_observations_can_reuse_a_mutation_id_without_replacing_its_saved_outcome() {
    let (_directory, store) = store().await;
    let original = request(Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }));
    let saved = store.execute(original.clone(), None).await.unwrap().result;
    let thread = saved["thread_id"].as_str().unwrap().to_owned();
    let read = Request::new(
        original.operation_id.clone(),
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.clone()),
        }),
    );
    let before = store.execute(read.clone(), None).await.unwrap().result;
    store
        .execute(
            request(Command::ThreadSet(Box::new(ThreadSetArgs {
                thread_id: Some(thread),
                title: Some("new title".into()),
                ..Default::default()
            }))),
            None,
        )
        .await
        .unwrap();
    let after = store.execute(read, None).await.unwrap().result;
    assert_ne!(before["title"], after["title"]);
    assert_eq!(after["title"], "new title");
    let lookup = store
        .execute(
            Request::new(
                original.operation_id.clone(),
                Command::OperationGet(OperationTarget {
                    operation_id: original.operation_id.clone(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(lookup["outcome"]["result"], saved);
    assert_eq!(
        store.execute(original.clone(), None).await.unwrap().result,
        saved
    );
    let mut conflict = original;
    if let Command::ThreadCreate(args) = &mut conflict.command {
        args.cwd = Some("different".into());
    }
    assert_eq!(
        store.execute(conflict, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn saved_usage_queries_and_gets_share_totals_without_receipts_or_schema_changes() {
    let (_root, store) = store().await;
    let parent = create(&store, None).await;
    let child = create(&store, Some(parent.thread_id.clone())).await;
    let p = parent.thread_id.clone();
    let c = child.thread_id.clone();
    store.database.call(move|db|{
        for (id,thread) in [("usage-parent",p),("usage-child",c)] {
            db.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?2,'completed','2026-10-30T00:00:00+00:00','2026-10-30T00:00:01+00:00')",rusqlite::params![id,thread])?;
            db.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES(?1,?2,?1,'completed','2026-10-30T00:00:00+00:00','2026-10-30T00:00:01+00:00')",rusqlite::params![id,thread])?;
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at,ended_at)VALUES(?1,?1,0,0,'test','fixture',0,'completed','{}','{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":0}}','2026-10-30T00:00:00+00:00','2026-10-30T00:00:00.010+00:00')",[id])?;
        }
        Ok(())
    }).await.unwrap();
    let before = store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM operation_receipt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    let own = execute(
        &store,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(parent.thread_id.clone()),
        }),
    )
    .await
    .result;
    assert_eq!(
        own["usage_totals"]["model_attempts"]["completed"]["input_tokens"]["known_sum"],
        10
    );
    let run = execute(
        &store,
        Command::RunGet(RunTarget {
            run_id: "usage-parent".into(),
        }),
    )
    .await
    .result;
    assert_eq!(run["usage_totals"]["run"]["duration_ms"]["known_sum"], 1000);
    let turn = execute(
        &store,
        Command::TurnGet(TurnTarget {
            turn_id: "usage-parent".into(),
        }),
    )
    .await
    .result;
    assert_eq!(turn["usage_totals"]["run"]["status"], "not_applicable");
    assert_eq!(turn["usage_totals"]["turn_lifetime_ms"], 1000);
    let req = request(Command::UsageGet(UsageGetArgs {
        thread_id: Some(parent.thread_id.clone().into_boxed_str()),
        include_children: true,
        ..Default::default()
    }));
    let first = store.execute(req.clone(), None).await.unwrap().result;
    assert_eq!(first["selected_threads"], 2);
    let parent_path = parent.thread_id.clone();
    let child_path = child.thread_id.clone();
    store.database.call(move |db| {
        db.execute("UPDATE model_attempt SET started_at='2026-10-30T00:00:01+00:00' WHERE id='usage-parent'", [])?;
        db.execute("UPDATE model_attempt SET started_at='2026-10-29T00:00:00+00:00' WHERE id='usage-child'", [])?;
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) VALUES('usage-marker','usage-parent','user','2026-10-30T00:00:00+00:00','{}',2)", [])?;
        db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,0,'usage-marker')", [&parent_path])?;
        db.execute("INSERT INTO node(id,turn_id,kind,created_at,payload_json,payload_bytes) VALUES('usage-child-marker','usage-child','user','2026-10-30T00:00:00+00:00','{}',2)", [])?;
        db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,0,'usage-child-marker')", [&child_path])?;
        Ok(())
    }).await.unwrap();
    let after_node = store
        .execute(
            request(Command::UsageGet(UsageGetArgs {
                thread_id: Some(parent.thread_id.clone().into_boxed_str()),
                since_node_id: Some(Box::from("usage-marker")),
                ..Default::default()
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(
        after_node["model_attempts"]["completed"]["input_tokens"]["known_sum"],
        10
    );
    let grouped_node = store
        .execute(
            request(Command::UsageGet(UsageGetArgs {
                thread_id: Some(parent.thread_id.clone().into_boxed_str()),
                since_node_id: Some(Box::from("usage-marker")),
                group_by: Some("thread".into()),
                ..Default::default()
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(
        grouped_node["items"].as_array().unwrap().len(),
        1,
        "{grouped_node}"
    );
    assert_eq!(grouped_node["items"][0]["key"], parent.thread_id);
    let until = store
        .execute(
            request(Command::UsageGet(UsageGetArgs {
                thread_id: Some(parent.thread_id.clone().into_boxed_str()),
                since_node_id: Some(Box::from("usage-marker")),
                until: Some("2026-10-30T00:00:00.500Z".into()),
                ..Default::default()
            })),
            None,
        )
        .await
        .unwrap()
        .result;
    assert!(until["model_attempts"].get("completed").is_none());
    let foreign_node = store
        .execute(
            request(Command::UsageGet(UsageGetArgs {
                thread_id: Some(parent.thread_id.clone().into_boxed_str()),
                since_node_id: Some(Box::from("usage-child-marker")),
                ..Default::default()
            })),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(foreign_node.code, ErrorCode::TargetUnavailable);
    assert_eq!(
        first["model_attempts"]["completed"]["input_tokens"]["known_sum"],
        20
    );
    store
        .database
        .call(|db| {
            db.execute(
                "UPDATE model_attempt SET usage_json=NULL WHERE id='usage-child'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let second = store.execute(req, None).await.unwrap().result;
    assert_eq!(
        second["model_attempts"]["completed"]["input_tokens"]["known_sum"],
        10
    );
    assert_eq!(
        second["model_attempts"]["completed"]["input_tokens"]["unknown_count"], 1,
        "read reuse must not replay a cached receipt"
    );
    let after = store
        .database
        .call(|db| {
            Ok((
                db.query_row("SELECT count(*) FROM operation_receipt", [], |r| {
                    r.get::<_, i64>(0)
                })?,
                db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(after.1, shellboy::storage::SCHEMA_VERSION);
    store.database.shutdown().await.unwrap();
}

fn set_auto_compact(thread: &str, value: &str) -> Command {
    Command::ThreadSet(Box::new(ThreadSetArgs {
        thread_id: Some(thread.into()),
        auto_compact_at: Some(value.into()),
        ..Default::default()
    }))
}

async fn get_thread(store: &Store, thread: &str) -> Value {
    execute(
        store,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(thread.into()),
        }),
    )
    .await
    .result
}

#[tokio::test]
async fn public_thread_get_reports_raw_spill_and_effective_default() {
    let (_root, store) = store().await;
    let thread = create(&store, None).await.thread_id;
    let details = get_thread(&store, &thread).await;
    assert!(
        details.get("spill").is_some(),
        "raw spill override must be present"
    );
    assert_eq!(details["spill"], Value::Null);
    assert_eq!(details["effective_spill"], true);

    let explicit_off = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            spill: Some("off".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(explicit_off.result["spill"], false);
    assert_eq!(explicit_off.result["effective_spill"], false);

    let null_is_omission = Request::from_value(serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "operation_id": "instance:null-spill-no-change",
        "command": "thread.set",
        "args": {"thread_id": thread.clone(), "title": "keep explicit off", "spill": null}
    }))
    .unwrap();
    store.execute(null_is_omission, None).await.unwrap();
    let unchanged = get_thread(&store, &thread).await;
    assert_eq!(
        unchanged["spill"], false,
        "null must not clear explicit off"
    );
    assert_eq!(unchanged["effective_spill"], false);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_auto_compact_override_sets_disables_clears_and_reports_effective_value() {
    let (_root, store) = store().await;
    let thread = create(&store, None).await.thread_id;
    let initial = get_thread(&store, &thread).await;
    assert_eq!(initial["auto_compact_at"], Value::Null);
    assert_eq!(initial["effective_auto_compact_at"], 200_000);
    let set = execute(&store, set_auto_compact(&thread, "50000")).await;
    assert_eq!(set.result["auto_compact_at"], 50_000);
    assert_eq!(set.result["effective_auto_compact_at"], 50_000);
    let got = get_thread(&store, &thread).await;
    assert_eq!(
        (
            got["auto_compact_at"].clone(),
            got["effective_auto_compact_at"].clone()
        ),
        (50_000.into(), 50_000.into())
    );
    execute(&store, set_auto_compact(&thread, "0")).await;
    let got = get_thread(&store, &thread).await;
    assert_eq!(
        (
            got["auto_compact_at"].clone(),
            got["effective_auto_compact_at"].clone()
        ),
        (0.into(), 0.into())
    );
    execute(&store, set_auto_compact(&thread, "")).await;
    let got = get_thread(&store, &thread).await;
    assert_eq!(got["auto_compact_at"], Value::Null);
    assert_eq!(got["effective_auto_compact_at"], 200_000);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_auto_compact_override_is_inherited_by_children_and_survives_connection_changes() {
    let (_root, store) = store().await;
    let parent = create(&store, None).await.thread_id;
    execute(&store, set_auto_compact(&parent, "1234")).await;
    let child = create(&store, Some(parent.clone())).await;
    assert_eq!(child.auto_compact_at, Some(1234));
    // A parent change after creation does not reach the existing child.
    execute(&store, set_auto_compact(&parent, "")).await;
    assert_eq!(
        get_thread(&store, &child.thread_id).await["auto_compact_at"],
        1234
    );
    let switched = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(child.thread_id.clone()),
            connection: Some("other".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(switched.result["connection"], "other");
    assert_eq!(switched.result["auto_compact_at"], 1234);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_auto_compact_rejects_invalid_values_like_other_thread_set_input() {
    let (_root, store) = store().await;
    let thread = create(&store, None).await.thread_id;
    execute(&store, set_auto_compact(&thread, "7")).await;
    for bad in ["-1", "1.5", "abc", " 5", "99999999999999999999"] {
        let error = store
            .execute(request(set_auto_compact(&thread, bad)), None)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad}");
    }
    assert_eq!(get_thread(&store, &thread).await["auto_compact_at"], 7);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_auto_compact_override_is_copied_by_fork() {
    let (_root, store) = store().await;
    let thread = create(&store, None).await.thread_id;
    execute(&store, set_auto_compact(&thread, "4321")).await;
    execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            spill: Some("off".into()),
            ..Default::default()
        })),
    )
    .await;
    let source = thread.clone();
    store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            shellboy::history::append_node(
                &tx,
                &source,
                "base",
                None,
                "user",
                &serde_json::json!({"role":"user","content":[{"type":"input_text","text":"base"}]}),
                "now",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let fork = execute(
        &store,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: thread.clone(),
            node_id: "base".into(),
            cwd: None,
        }),
    )
    .await;
    assert_eq!(fork.result["auto_compact_at"], 4321);
    assert_eq!(fork.result["spill"], false);
    assert_eq!(fork.result["effective_spill"], false);
    // Later changes to the source do not reach the fork.
    execute(&store, set_auto_compact(&thread, "")).await;
    execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            spill: Some("on".into()),
            ..Default::default()
        })),
    )
    .await;
    let fork_id = fork.result["thread_id"].as_str().unwrap();
    let child = get_thread(&store, fork_id).await;
    assert_eq!(child["auto_compact_at"], 4321);
    assert_eq!(child["spill"], false);
    assert_eq!(child["effective_spill"], false);
    store.database.shutdown().await.unwrap();
}

fn set_summary_parameters(thread: &str, chars: Option<&str>, percent: Option<&str>) -> Command {
    Command::ThreadSet(Box::new(ThreadSetArgs::summary_settings(
        thread.into(),
        percent.map(str::to_owned),
        chars.map(str::to_owned),
    )))
}

#[tokio::test]
async fn thread_summary_parameters_follow_default_connection_thread_precedence_and_clear() {
    let (_root, store) = store().await;
    let thread = create(&store, None).await.thread_id;
    let got = get_thread(&store, &thread).await;
    assert_eq!(got["summary_max_chars"], Value::Null);
    assert_eq!(got["effective_summary_max_chars"], 10_000);
    assert_eq!(got["effective_auto_compact_target_percent"], 20);
    // The connection value replaces [compaction] for a Thread without its own value.
    let moved = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            connection: Some("other".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(moved.result["effective_summary_max_chars"], 12_000);
    assert_eq!(moved.result["effective_auto_compact_target_percent"], 3);
    // The Thread value replaces the connection value and reports both.
    let set = execute(
        &store,
        set_summary_parameters(&thread, Some("8000"), Some("9")),
    )
    .await;
    assert_eq!(set.result["summary_max_chars"], 8_000);
    assert_eq!(set.result["effective_summary_max_chars"], 8_000);
    assert_eq!(set.result["auto_compact_target_percent"], 9);
    assert_eq!(set.result["effective_auto_compact_target_percent"], 9);
    // Each value changes independently; an empty string clears only that one.
    let cleared = execute(&store, set_summary_parameters(&thread, Some(""), None)).await;
    assert_eq!(cleared.result["summary_max_chars"], Value::Null);
    assert_eq!(cleared.result["effective_summary_max_chars"], 12_000);
    assert_eq!(cleared.result["auto_compact_target_percent"], 9);
    let got = get_thread(&store, &thread).await;
    assert_eq!(got["effective_summary_max_chars"], 12_000);
    assert_eq!(got["effective_auto_compact_target_percent"], 9);
    execute(&store, set_summary_parameters(&thread, None, Some(""))).await;
    assert_eq!(
        get_thread(&store, &thread).await["effective_auto_compact_target_percent"],
        3
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_summary_parameters_reject_out_of_range_values_and_are_inherited() {
    let (_root, store) = store().await;
    let thread = create(&store, None).await.thread_id;
    execute(
        &store,
        set_summary_parameters(&thread, Some("2000"), Some("16")),
    )
    .await;
    for (segment, rounds) in [
        (Some("0"), None),
        (Some("9223372036854775808"), None),
        (Some("-1"), None),
        (Some("1.5"), None),
        (Some(" 5000"), None),
        (None, Some("0")),
        (None, Some("101")),
        (None, Some("x")),
    ] {
        let error = store
            .execute(
                request(set_summary_parameters(&thread, segment, rounds)),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{segment:?} {rounds:?}"
        );
    }
    let got = get_thread(&store, &thread).await;
    assert_eq!(
        (
            got["summary_max_chars"].clone(),
            got["auto_compact_target_percent"].clone()
        ),
        (2_000.into(), 16.into())
    );
    // A child starts from its parent's values; later parent changes do not reach it.
    let child = create(&store, Some(thread.clone())).await;
    assert_eq!(
        (child.summary_max_chars, child.auto_compact_target_percent),
        (Some(2_000), Some(16))
    );
    execute(&store, set_summary_parameters(&thread, Some(""), Some(""))).await;
    let got = get_thread(&store, &child.thread_id).await;
    assert_eq!(
        (
            got["summary_max_chars"].clone(),
            got["auto_compact_target_percent"].clone()
        ),
        (2_000.into(), 16.into())
    );
    store.database.shutdown().await.unwrap();
}

async fn owned_send(store: &Store, origin: &Origin, thread: &str) -> MessageAccepted {
    let accepted = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                to_human: false,
                human: false,
                resume_recovery: false,
                expect_reply: false,
                sender_label: None,
                deliver: Deliver::Next,
                thread_id: Some(thread.into()),
                text: "delegated".into(),
            })),
            Some(origin.clone()),
        )
        .await
        .unwrap();
    serde_json::from_value(accepted.result).unwrap()
}

#[tokio::test]
async fn retry_has_no_authority_check_and_is_safe_when_raced() {
    let (_directory, store) = store().await;
    let grand = create(&store, None).await;
    let middle = create(&store, Some(grand.thread_id.clone())).await;
    let leaf = create(&store, Some(middle.thread_id.clone())).await;
    let (grand_run, _) = send(&store, &grand.thread_id, "grand").await;
    let grand_origin = caller(
        &grand.thread_id,
        grand_run.run_id.as_deref().unwrap(),
        grand_run.turn_id.as_deref().unwrap(),
    );
    let middle_run = owned_send(&store, &grand_origin, &middle.thread_id).await;
    let middle_origin = caller(
        &middle.thread_id,
        middle_run.run_id.as_deref().unwrap(),
        middle_run.turn_id.as_deref().unwrap(),
    );
    let leaf_run = owned_send(&store, &middle_origin, &leaf.thread_id)
        .await
        .run_id
        .unwrap();
    let stranger = create(&store, None).await;
    let (stranger_run, _) = send(&store, &stranger.thread_id, "stranger").await;
    let stranger_origin = caller(
        &stranger.thread_id,
        stranger_run.run_id.as_deref().unwrap(),
        stranger_run.turn_id.as_deref().unwrap(),
    );
    let to_recovery = |run: String| {
        store.database.call(move |db| {
            db.execute(
                "UPDATE turn SET state='recovery_wait' WHERE run_id=?1",
                [run],
            )?;
            Ok(())
        })
    };
    to_recovery(leaf_run.clone()).await.unwrap();
    let retry = || {
        request(Command::RunRetry(RunTarget {
            run_id: leaf_run.clone(),
        }))
    };
    // A non-owner Run retries through the same path as a human: no authority check.
    // Two callers (owner and a stranger) race one retry: exactly one resumes the Turn.
    let (direct, ancestor) = tokio::join!(
        store.execute(retry(), Some(stranger_origin)),
        store.execute(retry(), Some(middle_origin.clone()))
    );
    assert_eq!(
        direct.is_ok() as u8 + ancestor.is_ok() as u8,
        1,
        "one retry wins, the other sees the changed Turn"
    );
    let loser = direct.as_ref().err().or(ancestor.as_ref().err()).unwrap();
    assert_eq!(loser.code, ErrorCode::InvalidState);
    let winner = direct.or(ancestor).unwrap();
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM pragma_table_info('run') WHERE name='owner_run_id'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(winner.quiet_wake_threads, vec![leaf.thread_id.clone()]);
    // Replaying the winning operation ID returns the saved result without resuming again.
    let again = retry();
    to_recovery(leaf_run.clone()).await.unwrap();
    let first = store
        .execute(again.clone(), Some(grand_origin.clone()))
        .await
        .unwrap();
    let replay = store
        .execute(again, Some(grand_origin.clone()))
        .await
        .unwrap();
    assert_eq!(replay.result, first.result);
    assert!(replay.quiet_wake_threads.is_empty());
    // A human retry still works.
    to_recovery(leaf_run.clone()).await.unwrap();
    execute(
        &store,
        Command::RunRetry(RunTarget {
            run_id: leaf_run.clone(),
        }),
    )
    .await;
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn single_summary_settings_inherit_clear_and_reopen() {
    let (root, store) = store().await;
    let parent = create(&store, None).await;
    let set = ThreadSetArgs {
        thread_id: Some(parent.thread_id.clone()),
        auto_compact_target_percent: Some("80".into()),
        summary_max_chars: Some("12345".into()),
        ..Default::default()
    };
    let result = execute(&store, Command::ThreadSet(Box::new(set))).await;
    assert_eq!(result.result["effective_summary_max_chars"], 12345);
    let child = create(&store, Some(parent.thread_id.clone())).await;
    assert_eq!(child.auto_compact_target_percent, Some(80));
    assert_eq!(child.summary_max_chars, Some(12345));
    let clear = ThreadSetArgs {
        thread_id: Some(parent.thread_id.clone()),
        auto_compact_at: Some("0".into()),
        auto_compact_target_percent: Some(String::new()),
        summary_max_chars: Some(String::new()),
        ..Default::default()
    };
    let result = execute(&store, Command::ThreadSet(Box::new(clear))).await;
    assert!(result.result["summary_max_chars"].is_null());
    assert_eq!(result.result["effective_summary_max_chars"], 10000); // connection value precedes the default
    assert_eq!(result.result["effective_auto_compact_target_percent"], 20);
    assert_eq!(result.result["effective_auto_compact_at"], 0);
    let config = store.config.clone();
    store.database.shutdown().await.unwrap();
    let reopened = Store::new(
        DbWorker::open(root.path().join("db")).await.unwrap(),
        config,
        root.path().join("project"),
        "reopen".into(),
    );
    let result = execute(
        &reopened,
        Command::ThreadGet(ThreadTarget {
            thread_id: Some(child.thread_id),
        }),
    )
    .await;
    assert_eq!(result.result["auto_compact_target_percent"], 80);
    assert_eq!(result.result["summary_max_chars"], 12345);
    reopened.database.shutdown().await.unwrap();
}

async fn max_tokens_store() -> (TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let database = DbWorker::open(directory.path().join("db")).await.unwrap();
    let config = Config::parse(
        r#"
        default_connection="plain"
        default_model="fixture"
        [connections.plain]
        base_url="http://127.0.0.1:1/v1"
        [connections.limited]
        base_url="http://127.0.0.1:2/v1"
        max_tokens=4096
        [connections.messages]
        api="anthropic_messages"
        base_url="http://127.0.0.1:3/v1"
        [connections.passthrough]
        base_url="http://127.0.0.1:4/v1"
        extra_body={max_output_tokens=777}
    "#,
    )
    .unwrap();
    let startup_cwd = directory.path().join("project");
    (
        directory,
        Store::new(database, Arc::new(config), startup_cwd, "instance".into()),
    )
}

fn set_max_tokens(thread: &str, value: &str) -> Command {
    Command::ThreadSet(Box::new(ThreadSetArgs {
        thread_id: Some(thread.into()),
        max_tokens: Some(value.into()),
        ..Default::default()
    }))
}

fn switch_connection(thread: &str, connection: &str) -> Command {
    Command::ThreadSet(Box::new(ThreadSetArgs {
        thread_id: Some(thread.into()),
        connection: Some(connection.into()),
        ..Default::default()
    }))
}

#[tokio::test]
async fn thread_max_tokens_overrides_connection_value_survives_switch_and_clears() {
    let (_root, store) = max_tokens_store().await;
    let thread = create(&store, None).await.thread_id;
    // Unset everywhere: Responses sends no limit field, so nothing is effective.
    let got = get_thread(&store, &thread).await;
    assert_eq!(got["max_tokens"], Value::Null);
    assert!(got.get("effective_max_tokens").is_none(), "{got}");
    let moved = execute(&store, switch_connection(&thread, "limited")).await;
    assert_eq!(moved.result["effective_max_tokens"], 4096);
    let moved = execute(&store, switch_connection(&thread, "messages")).await;
    assert_eq!(moved.result["effective_max_tokens"], 32768);
    // The Thread value wins over the connection and the Messages default.
    let set = execute(&store, set_max_tokens(&thread, "64000")).await;
    assert_eq!(set.result["max_tokens"], 64000);
    assert_eq!(set.result["effective_max_tokens"], 64000);
    let moved = execute(&store, switch_connection(&thread, "limited")).await;
    assert_eq!(moved.result["max_tokens"], 64000);
    assert_eq!(moved.result["effective_max_tokens"], 64000);
    assert_eq!(
        get_thread(&store, &thread).await["effective_max_tokens"],
        64000
    );
    // An empty string clears only the Thread value.
    let cleared = execute(&store, set_max_tokens(&thread, "")).await;
    assert_eq!(cleared.result["max_tokens"], Value::Null);
    assert_eq!(cleared.result["effective_max_tokens"], 4096);
    assert_eq!(cleared.result["connection"], "limited");
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_max_tokens_rejects_non_positive_or_non_integer_and_is_inherited() {
    let (_root, store) = max_tokens_store().await;
    let thread = create(&store, None).await.thread_id;
    execute(&store, set_max_tokens(&thread, "9000")).await;
    for bad in ["0", "-1", "1.5", "abc", " 5", "9223372036854775808"] {
        let error = store
            .execute(request(set_max_tokens(&thread, bad)), None)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad}");
    }
    assert_eq!(get_thread(&store, &thread).await["max_tokens"], 9000);
    // The upper bound is the provider's, not Shellboy's.
    execute(&store, set_max_tokens(&thread, "9223372036854775807")).await;
    execute(&store, set_max_tokens(&thread, "9000")).await;
    // A child and a fork start from the source value; later changes do not reach them.
    let child = create(&store, Some(thread.clone())).await;
    assert_eq!(child.max_tokens, Some(9000));
    let source = thread.clone();
    store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            shellboy::history::append_node(
                &tx,
                &source,
                "base",
                None,
                "user",
                &serde_json::json!({"role":"user","content":[{"type":"input_text","text":"base"}]}),
                "now",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    let fork = execute(
        &store,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: thread.clone(),
            node_id: "base".into(),
            cwd: None,
        }),
    )
    .await;
    assert_eq!(fork.result["max_tokens"], 9000);
    let fork_id = fork.result["thread_id"].as_str().unwrap().to_owned();
    execute(&store, set_max_tokens(&thread, "")).await;
    assert_eq!(
        get_thread(&store, &child.thread_id).await["max_tokens"],
        9000
    );
    assert_eq!(get_thread(&store, &fork_id).await["max_tokens"], 9000);
    assert_eq!(get_thread(&store, &thread).await["max_tokens"], Value::Null);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn thread_max_tokens_refuses_a_clash_with_connection_extra_body_limit() {
    let (_root, store) = max_tokens_store().await;
    let thread = create(&store, None).await.thread_id;
    execute(&store, set_max_tokens(&thread, "9000")).await;
    let refused = |error: ApiError| {
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(
            error.message.contains("extra_body.max_output_tokens")
                && error.message.contains("passthrough")
                && error.message.contains("--max-tokens ''"),
            "{}",
            error.message
        );
    };
    // Switching to a connection whose extra_body carries the native key is refused.
    refused(
        store
            .execute(request(switch_connection(&thread, "passthrough")), None)
            .await
            .unwrap_err(),
    );
    let got = get_thread(&store, &thread).await;
    assert_eq!(
        (got["connection"].clone(), got["max_tokens"].clone()),
        ("plain".into(), 9000.into())
    );
    // Clearing the Thread value in the same call makes the switch valid.
    let switched = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(thread.clone()),
            connection: Some("passthrough".into()),
            max_tokens: Some(String::new()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(switched.result["connection"], "passthrough");
    // extra_body passthrough is not a Shellboy-sent limit; set and get agree.
    assert!(
        switched.result.get("effective_max_tokens").is_none(),
        "{}",
        switched.result
    );
    assert!(
        get_thread(&store, &thread)
            .await
            .get("effective_max_tokens")
            .is_none()
    );
    // Setting a Thread value on that connection is refused too; other edits still work.
    refused(
        store
            .execute(request(set_max_tokens(&thread, "9000")), None)
            .await
            .unwrap_err(),
    );
    assert_eq!(get_thread(&store, &thread).await["max_tokens"], Value::Null);
    execute(&store, switch_connection(&thread, "limited")).await;
    execute(&store, set_max_tokens(&thread, "9000")).await;
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn spill_precedence_inheritance_revision_and_rollback_are_public() {
    let (_root, store) = spill_store().await;
    let create_off = ThreadCreateArgs {
        agents: None,
        connection: Some("off".into()),
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    };
    let parent: ThreadDetails = serde_json::from_value(
        execute(&store, Command::ThreadCreate(create_off.clone()))
            .await
            .result,
    )
    .unwrap();
    assert_eq!(parent.spill, None);
    assert!(!parent.effective_spill);

    let connection_on: ThreadDetails = serde_json::from_value(
        execute(
            &store,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: Some("on".into()),
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        )
        .await
        .result,
    )
    .unwrap();
    assert_eq!(connection_on.spill, None);
    assert!(connection_on.effective_spill);

    let before = thread_revisions(&store, &parent.thread_id).await;
    let turned_off = execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(parent.thread_id.clone()),
            spill: Some("off".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(turned_off.result["spill"], false);
    assert_eq!(turned_off.result["effective_spill"], false);
    let after = thread_revisions(&store, &parent.thread_id).await;
    assert_eq!(after.0, before.0 + 1);
    assert_eq!((after.1, after.2, after.3), (before.1, before.2, before.3));

    let child: ThreadDetails = serde_json::from_value(
        execute(
            &store,
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: Some(parent.thread_id.clone()),
            }),
        )
        .await
        .result,
    )
    .unwrap();
    assert_eq!(child.spill, Some(false));
    assert!(!child.effective_spill);

    let failed = store
        .execute(
            request(Command::ThreadSet(Box::new(ThreadSetArgs {
                thread_id: Some(parent.thread_id.clone()),
                connection: Some("missing".into()),
                spill: Some("on".into()),
                ..Default::default()
            }))),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(failed.code, ErrorCode::ConfigInvalid);
    assert_eq!(get_thread(&store, &parent.thread_id).await["spill"], false);
    assert_eq!(
        get_thread(&store, &parent.thread_id).await["effective_spill"],
        false
    );

    execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(parent.thread_id.clone()),
            spill: Some(String::new()),
            ..Default::default()
        })),
    )
    .await;
    let cleared = get_thread(&store, &parent.thread_id).await;
    assert_eq!(cleared["spill"], Value::Null);
    assert_eq!(cleared["effective_spill"], false);

    let invalid = store
        .execute(
            request(Command::ThreadSet(Box::new(ThreadSetArgs {
                thread_id: Some(parent.thread_id.clone()),
                spill: Some("bad".into()),
                ..Default::default()
            }))),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.code, ErrorCode::InvalidArgument);
    assert_eq!(
        get_thread(&store, &parent.thread_id).await["spill"],
        Value::Null
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_inherits_raw_summary_overrides_and_clear_restores_connection_defaults() {
    let (_root, store) = store().await;
    let parent = create(&store, None).await.thread_id;
    let source_parent = parent.clone();
    store
        .database
        .call(move |db| {
            let tx = db.transaction()?;
            shellboy::history::append_node(
                &tx,
                &source_parent,
                "fork-settings-source",
                None,
                "user",
                &serde_json::json!({"role":"user","content":"source"}),
                "now",
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    execute(
        &store,
        set_summary_parameters(&parent, Some("4000"), Some("80")),
    )
    .await;
    let result = execute(
        &store,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: parent.clone(),
            node_id: "fork-settings-source".into(),
            cwd: None,
        }),
    )
    .await;
    let branch = result.result["thread_id"].as_str().unwrap().to_owned();
    let inherited = get_thread(&store, &branch).await;
    assert_eq!(inherited["summary_max_chars"], 4000);
    assert_eq!(inherited["auto_compact_target_percent"], 80);
    execute(&store, set_summary_parameters(&parent, Some(""), Some(""))).await;
    assert_eq!(get_thread(&store, &branch).await["summary_max_chars"], 4000);
    execute(
        &store,
        Command::ThreadSet(Box::new(ThreadSetArgs {
            thread_id: Some(branch.clone()),
            connection: Some("other".into()),
            ..Default::default()
        })),
    )
    .await;
    assert_eq!(
        get_thread(&store, &branch).await["effective_summary_max_chars"],
        4000
    );
    let cleared = execute(&store, set_summary_parameters(&branch, Some(""), Some(""))).await;
    assert_eq!(cleared.result["summary_max_chars"], Value::Null);
    assert_eq!(cleared.result["auto_compact_target_percent"], Value::Null);
    assert_eq!(cleared.result["effective_summary_max_chars"], 12000);
    assert_eq!(cleared.result["effective_auto_compact_target_percent"], 3);
    let cleared_branch = execute(
        &store,
        Command::ThreadFork(ThreadForkArgs {
            agents: None,
            thread_id: branch.clone(),
            node_id: "fork-settings-source".into(),
            cwd: None,
        }),
    )
    .await;
    let null_child = get_thread(&store, cleared_branch.result["thread_id"].as_str().unwrap()).await;
    assert_eq!(null_child["summary_max_chars"], Value::Null);
    assert_eq!(null_child["auto_compact_target_percent"], Value::Null);
    assert_eq!(null_child["effective_summary_max_chars"], 12000);
    assert_eq!(null_child["effective_auto_compact_target_percent"], 3);
    assert_eq!(
        get_thread(&store, &parent).await["effective_summary_max_chars"],
        10000
    );
    store.database.shutdown().await.unwrap();
}
