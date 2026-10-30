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
    "#,
    )
    .unwrap();
    let startup_cwd = directory.path().join("project");
    (
        directory,
        Store::new(database, Arc::new(config), startup_cwd, "instance".into()),
    )
}
fn request(command: Command) -> Request {
    Request::new(format!("instance:{}", uuid::Uuid::new_v4()), command)
}
async fn execute(store: &Store, command: Command) -> MutationResult {
    store.execute(request(command), None).await.unwrap()
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
        thread_id: Some(thread.thread_id.clone()),
        text: "fixture".into(),
    }));
    let first = store.execute(req.clone(), None).await.unwrap();
    let second = store.execute(req.clone(), None).await.unwrap();
    assert_eq!(first.result, second.result);
    assert_eq!(first.wake_threads, vec![thread.thread_id]);
    assert!(
        second.wake_threads.is_empty(),
        "retransmission must not reschedule effects"
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
            Command::ThreadSet(ThreadSetArgs {
                agents: None,
                title: None,
                thread_id: Some(id.clone()),
                connection: None,
                cwd: Some("nested/..".into()),
                model: Some(model.into()),
                effort: None,
            }),
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
    execute(
        &store,
        Command::ThreadDelete(ThreadDeleteArgs {
            thread_id: thread.thread_id.clone(),
            descendants: false,
        }),
    )
    .await;
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
                thread_id: Some(child.thread_id.clone()),
                text: "ordinary parent note".into(),
            })),
            Some(origin),
        )
        .await
        .unwrap();
    let mechanical_message: MessageAccepted = serde_json::from_value(mechanical.result).unwrap();
    assert_eq!(mechanical_message.delivery_status, DeliveryStatus::Held);
    assert!(mechanical.wake_threads.is_empty());
    let (human, effects) = send(&store, &child.thread_id, "human resume instruction").await;
    assert_eq!(human.turn_id, accepted.turn_id);
    assert_eq!(human.delivery_status, DeliveryStatus::Accepted);
    assert_eq!(effects.wake_threads, vec![child.thread_id]);
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
async fn hierarchical_stop_freezes_owned_descendants_before_new_child_start() {
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
    assert_eq!(
        stop.stop_runs,
        vec![
            child_message.run_id.unwrap(),
            parent_message.run_id.clone().unwrap()
        ],
        "children stop before parents"
    );
    let new_child = create(&store, Some(parent.thread_id)).await;
    let result = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
                thread_id: Some(new_child.thread_id),
                text: "forbidden late child".into(),
            })),
            Some(origin),
        )
        .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::InvalidState);
    store
        .database
        .call(|connection| {
            assert_eq!(
                connection.query_row(
                    "SELECT count(*) FROM run WHERE state='stopping'",
                    [],
                    |row| row.get::<_, i64>(0)
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
async fn receipt_failure_rolls_back_message_run_turn_and_caches_error_without_effects() {
    let (_directory, store) = store().await;
    let thread = create(&store, None).await;
    store.database.call(|connection| {connection.execute_batch("CREATE TRIGGER fixture_reject_receipt BEFORE INSERT ON receipt BEGIN SELECT RAISE(ABORT,'fixture save failure'); END;")?;Ok(())}).await.unwrap();
    let req = request(Command::MessageSend(MessageSendArgs {
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
            })),
            None,
        )
        .await;
    assert_eq!(wrong.unwrap_err().code, ErrorCode::CursorExpired);
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
        Command::ThreadSet(ThreadSetArgs {
            agents: None,
            title: None,
            thread_id: Some(thread.thread_id.clone()),
            connection: None,
            model: None,
            effort: Some("high".into()),
            cwd: None,
        }),
    )
    .await;
    assert_eq!(changed.result["effort"], "high");
    let changed = execute(
        &store,
        Command::ThreadSet(ThreadSetArgs {
            agents: None,
            title: None,
            thread_id: Some(thread.thread_id.clone()),
            connection: None,
            model: Some("next-model".into()),
            effort: None,
            cwd: None,
        }),
    )
    .await;
    assert_eq!(changed.result["effort"], "high");
    let cleared = execute(
        &store,
        Command::ThreadSet(ThreadSetArgs {
            agents: None,
            title: None,
            thread_id: Some(thread.thread_id),
            connection: None,
            model: None,
            effort: Some(String::new()),
            cwd: None,
        }),
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
async fn model_stop_authority_follows_run_ownership_instead_of_thread_genealogy() {
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
    for run in [
        independent_message.run_id.as_ref().unwrap(),
        unowned_message.run_id.as_ref().unwrap(),
    ] {
        let denied = store
            .execute(
                request(Command::RunStop(RunTarget {
                    run_id: run.clone(),
                })),
                Some(origin.clone()),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.code, ErrorCode::InvalidState);
        let id = run.clone();
        store
            .database
            .call(move |db| {
                assert_eq!(
                    db.query_row("SELECT state FROM run WHERE id=?1", [id], |r| r
                        .get::<_, String>(0))?,
                    "live"
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    let owned = create(&store, Some(parent.thread_id.clone())).await;
    let accepted = store
        .execute(
            request(Command::MessageSend(MessageSendArgs {
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
    assert_eq!(
        stopped.stop_runs,
        vec![child.run_id.unwrap(), parent_message.run_id.unwrap()]
    );
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
                assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), expected);
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
    assert_eq!(store.collect_receipts().await.unwrap(), 0);
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
    assert_eq!(store.collect_receipts().await.unwrap(), 1);
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
    assert_eq!(store.collect_receipts().await.unwrap(), 1);
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn repeated_quota_refusals_commit_bounded_housekeeping_without_accepting_new_work() {
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
    for (n, remaining) in [172, 44, 0].into_iter().enumerate() {
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
                    remaining
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
                    thread_id: Some(thread.thread_id.clone()),
                    text: "explicit human recovery".into(),
                })
            };
            let result = store.execute(request(command), None).await;
            if suppress {
                assert_eq!(
                    result.unwrap_err().code,
                    if retry {
                        ErrorCode::InvalidState
                    } else {
                        ErrorCode::OperationConflict
                    }
                );
            } else {
                let effects = result.unwrap();
                if retry {
                    assert!(effects.wake_threads.is_empty());
                    assert_eq!(effects.quiet_wake_threads, vec![thread.thread_id.clone()]);
                } else {
                    assert_eq!(effects.wake_threads, vec![thread.thread_id.clone()]);
                    assert!(effects.quiet_wake_threads.is_empty());
                }
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
            request(Command::ThreadSet(ThreadSetArgs {
                thread_id: Some(thread),
                title: Some("new title".into()),
                ..Default::default()
            })),
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
