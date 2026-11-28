//! Synthetic operation receipts: command acceptance is immutable, downstream work is separate.
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    config::Config,
    control::db::{RuntimeReservation, Store},
    protocol::*,
    storage::DbWorker,
};
use std::sync::Arc;

async fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let db = DbWorker::open(dir.path().join("fixture.db")).await.unwrap();
    let config = Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
    let store = Store::new(db, Arc::new(config), dir.path().into(), "first".into());
    (dir, store)
}
fn request(instance: &str, command: Command) -> Request {
    Request::new(format!("{instance}:{}", uuid::Uuid::new_v4()), command)
}
async fn inspect(store: &Store, id: &str) -> Value {
    store
        .execute(
            request(
                &store.operation_epoch.current(),
                Command::OperationGet(OperationTarget {
                    operation_id: id.into(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result
}
async fn thread(store: &Store) -> String {
    store
        .execute(
            request(
                &store.operation_epoch.current(),
                Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    parent_thread_id: None,
                    cwd: None,
                    connection: None,
                    model: None,
                    effort: None,
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result["thread_id"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test]
async fn explicit_context_edit_keeps_each_intent_in_the_protected_original_receipt() {
    let (_root, store) = fixture().await;
    let thread = thread(&store).await;
    let edit = |action, from: Option<&str>, to: Option<&str>| {
        Command::ContextEdit(ContextEditArgs {
            max_chars: None,
            thread_id: Some(thread.clone()),
            action,
            from_node_id: from.map(str::to_owned),
            to_node_id: to.map(str::to_owned),
        })
    };
    let first = request(
        "first.0",
        edit(ContextEditAction::Spill, Some("a"), Some("b")),
    );
    let second = request("first.0", edit(ContextEditAction::Trim, None, Some("c")));
    let accepted = store.execute(first.clone(), None).await.unwrap();
    let other = store.execute(second.clone(), None).await.unwrap();
    assert_eq!(accepted.result["context_edit"]["action"], "spill");
    assert_eq!(other.result["context_edit"]["action"], "trim");
    assert_eq!(
        store.execute(first.clone(), None).await.unwrap().result,
        accepted.result
    );
    let changed = Request::new(first.operation_id.clone(), second.command.clone());
    assert_eq!(
        store.execute(changed, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    let pending_thread = thread.clone();
    let ids = store
        .database
        .call(move |db| {
            let body: String = db.query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [format!("compact.pending.{pending_thread}")],
                |row| row.get(0),
            )?;
            Ok(serde_json::from_str::<Vec<String>>(&body).unwrap())
        })
        .await
        .unwrap();
    assert_eq!(
        ids,
        vec![first.operation_id.clone(), second.operation_id.clone()],
        "independent actions stay ordered; replay does not append again"
    );
    for operation in [&first.operation_id, &second.operation_id] {
        let details = inspect(&store, operation).await;
        assert_eq!(details["downstream"]["state"], "queued");
    }
    let older = first.operation_id.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE operation_receipt SET created_at='2000-01-01' WHERE operation_id=?1",
                [older],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store.collect_receipts().await.unwrap();
    assert_eq!(
        store.execute(first, None).await.unwrap().result,
        accepted.result,
        "pending receipt protects the stored edit arguments from expiry"
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn stopping_thread_remains_observable_when_context_metrics_are_temporarily_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = shellboy::control::Control::new(&owner, root.path().into());
    let created = control
        .request(request(
            &owner.operation_epoch.current(),
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                cwd: None,
                connection: None,
                model: None,
                effort: None,
                parent_thread_id: None,
            }),
        ))
        .await;
    assert!(created.ok, "{created:?}");
    let thread = created.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stopped_thread = thread.clone();
    owner.db.call(move|db| {
        db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('stopping-run',?1,'stopping','now')",[stopped_thread])?;
        Ok(())
    }).await.unwrap();
    let details = control
        .request(request(
            &owner.operation_epoch.current(),
            Command::ThreadGet(shellboy::protocol::ThreadGetArgs {
                thread_id: Some(thread.clone()),
                display: false,
            }),
        ))
        .await;
    assert!(
        details.ok,
        "stopping metadata remains readable: {details:?}"
    );
    let result = details.result.unwrap();
    assert_eq!(result["thread_id"], thread);
    assert_eq!(result["run_id"], "stopping-run");
    assert!(result["context_usage"]["last_successful_attempt"].is_null());
    assert!(
        result["context_usage"]
            .get("estimated_input_tokens")
            .is_none()
    );
    let decoded: ThreadDetails = serde_json::from_value(result).unwrap();
    assert!(decoded.context_usage.is_some());
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_response_replays_original_acceptance_after_message_progress_and_restart() {
    let (_dir, store) = fixture().await;
    let thread = thread(&store).await;
    let req = request(
        "first.0",
        Command::MessageSend(MessageSendArgs {
            max_model_requests: None,
            resume_recovery: false,

            human: false,
            sender_label: None,
            deliver: Deliver::Next,
            thread_id: Some(thread.clone()),
            text: "fixture request".into(),
        }),
    );
    let original = store.execute(req.clone(), None).await.unwrap().result;
    let message = original["message_id"].as_str().unwrap().to_owned();
    let message2 = message.clone();
    store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE receipt SET state='applied' WHERE message_node_id=?1",
                [message2],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let newer = Store::new(
        store.database.clone(),
        store.config.clone(),
        store.startup_cwd.clone(),
        "second".into(),
    );
    let status = inspect(&newer, &req.operation_id).await;
    assert_eq!(status["state"], "completed");
    assert_eq!(status["outcome"]["result"], original);
    assert_eq!(status["downstream"]["state"], "applied");
    assert_eq!(status["targets"]["message_id"], message);
    assert_eq!(status["current_instance"], false);
    assert_eq!(status["result_uncertain"], false);
    assert!(status["created_at"].as_str().is_some());
    assert_eq!(
        newer.execute(req.clone(), None).await.unwrap().result,
        original
    );
    let mut conflicting = req;
    conflicting.command = Command::MessageSend(MessageSendArgs {
        max_model_requests: None,
        resume_recovery: false,

        human: false,
        sender_label: None,
        deliver: Deliver::Next,
        thread_id: Some(thread),
        text: "changed".into(),
    });
    assert_eq!(
        newer.execute(conflicting, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM receipt", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_external_intent_is_uncertain_and_never_reexecuted_by_a_new_instance() {
    let (_dir, store) = fixture().await;
    let req = request(
        "first.0",
        Command::ShellWrite(ShellWriteArgs {
            execution_id: "synthetic".into(),
            text: "write once".into(),
            encoding: "UTF-8".into(),
        }),
    );
    assert!(matches!(
        store.reserve_runtime(req.clone(), None).await.unwrap(),
        RuntimeReservation::New(_)
    ));
    let newer = Store::new(
        store.database.clone(),
        store.config.clone(),
        store.startup_cwd.clone(),
        "second".into(),
    );
    let status = inspect(&newer, &req.operation_id).await;
    assert_eq!(status["state"], "pending");
    assert_eq!(status["result_uncertain"], true);
    assert_eq!(status["replay_policy"], "do_not_reexecute");
    assert_eq!(
        status["targets"],
        json!({}),
        "pending intent cannot fabricate target IDs"
    );
    assert!(matches!(
        newer.reserve_runtime(req.clone(), None).await.unwrap(),
        RuntimeReservation::Pending
    ));
    let mut unknown = req;
    unknown.operation_id = "first:unknown".into();
    assert_eq!(
        newer.reserve_runtime(unknown, None).await.unwrap_err().code,
        ErrorCode::OperationExpired
    );
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_compaction_restart_keeps_acceptance_and_records_interrupted_downstream() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
    let data = dir.path().join("data");
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), dir.path().into())
        .await
        .unwrap();
    let store=Store::new(owner.db.clone(),Arc::new(Config::parse("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap()),dir.path().into(),owner.instance_id.clone());
    let thread = thread(&store).await;
    let req = request(
        &store.operation_epoch.current(),
        Command::Compact(ThreadTarget {
            thread_id: Some(thread),
        }),
    );
    let accepted = store.execute(req.clone(), None).await.unwrap().result;
    assert_eq!(
        inspect(&store, &req.operation_id).await["downstream"]["state"],
        "queued"
    );
    let config_arc = store.config.clone();
    drop(store);
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(Some(config), Some(data), dir.path().into())
        .await
        .unwrap();
    let store = Store::new(
        owner.db.clone(),
        config_arc,
        dir.path().into(),
        owner.instance_id.clone(),
    );
    let status = inspect(&store, &req.operation_id).await;
    assert_eq!(status["state"], "completed");
    assert_eq!(status["outcome"]["result"], accepted);
    assert_eq!(status["downstream"]["state"], "failed");
    assert_eq!(status["downstream"]["result_uncertain"], true);
    assert_eq!(store.execute(req, None).await.unwrap().result, accepted);
    drop(store);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn attachment_replay_precedes_deleted_target_preflight_and_checks_content_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(dir.path().join("data")),
        dir.path().into(),
    )
    .await
    .unwrap();
    let control = shellboy::control::Control::new(&owner, dir.path().into());
    let thread = thread(&control.store).await;
    let file = dir.path().join("synthetic.txt");
    std::fs::write(&file, "synthetic attachment").unwrap();
    let req = request(
        &owner.operation_epoch.current(),
        Command::AttachmentAdd(AttachmentAddArgs {
            thread_id: Some(thread.clone()),
            path: file.to_string_lossy().into(),
            kind: AttachmentKind::File,
        }),
    );
    let first = control.request(req.clone()).await;
    assert!(first.ok, "{:?}", first.error);
    let deleted = control
        .request(request(
            &owner.operation_epoch.current(),
            Command::ThreadDelete(ThreadDeleteArgs {
                preview_marker: None,
                thread_id: thread,
                descendants: false,
            }),
        ))
        .await;
    assert!(deleted.ok, "{:?}", deleted.error);
    std::fs::remove_file(file).unwrap();
    let replay = control.request(req.clone()).await;
    assert!(replay.ok, "{:?}", replay.error);
    assert_eq!(replay.result, first.result);
    let mut conflict = req;
    if let Command::AttachmentAdd(ref mut args) = conflict.command {
        args.path = "different path".into();
    }
    assert_eq!(
        control.request(conflict).await.error.unwrap().code,
        ErrorCode::OperationConflict
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipt_capacity_rejection_does_not_stop_owner_or_block_internal_gc_and_lookup() {
    use shellboy::control::Control;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    let instance = control.store.instance_id.clone();
    let pending = format!("{instance}:pending-fixture");
    owner.db.call({let pending=pending.clone();move|db|{
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes)VALUES(?1,?2,'null','fixture','pending','2026-10-25T00:00:00Z',268435456)",rusqlite::params![pending,instance])?;
        Ok(())
    }}).await.unwrap();
    let epoch = control.store.operation_epoch.current();
    let rejected = control
        .request(request(
            &epoch,
            Command::StorageCollect(StorageArgs { category: None }),
        ))
        .await;
    let rejected = rejected.error.unwrap();
    assert_eq!(rejected.code, ErrorCode::StorageLimit);
    // Only protected intent holds the space: the epoch does not move.
    assert!(
        rejected
            .details
            .is_none_or(|d| d["reason"] != "operation_epoch_rolled")
    );
    assert_eq!(control.store.operation_epoch.current(), epoch);
    let lookup = control
        .request(request(
            &control.store.operation_epoch.current(),
            Command::OperationGet(OperationTarget {
                operation_id: pending.clone(),
            }),
        ))
        .await;
    assert!(
        lookup.ok,
        "Capacity rejection must not enter save-failed shutdown: {:?}",
        lookup.error
    );
    assert_eq!(lookup.result.unwrap()["state"], "pending");
    control
        .gc
        .collect(StorageCategory::All, chrono::Utc::now())
        .await
        .unwrap();
    control.store.collect_receipts().await.unwrap();
    owner
        .db
        .call(move |db| {
            db.execute(
                "UPDATE operation_receipt SET reserved_bytes=0 WHERE operation_id=?1",
                [pending],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let list = control
        .request(request(
            &control.store.operation_epoch.current(),
            Command::ThreadList(ThreadListArgs {
                root: None,
                include_descendant_count: false,
                include_activity: false,
                include_detail: false,
                thread_ids: None,
                state: None,
                order: Default::default(),
                limit: 50,
                cursor: None,
                query: None,
                awaiting: None,
                archived: Default::default(),
                tags: Vec::new(),
                cwd: None,
            }),
        ))
        .await;
    assert!(
        list.ok,
        "Owner still accepts work after capacity returns: {:?}",
        list.error
    );
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn full_receipt_budget_keeps_replay_status_gc_epoch_roll_and_owner_restart_available() {
    use shellboy::control::Control;
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
    let data = root.path().join("data");
    let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    let saved_request = request(
        &control.store.operation_epoch.current(),
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            parent_thread_id: None,
            cwd: None,
            connection: None,
            model: None,
            effort: None,
        }),
    );
    let saved = control.request(saved_request.clone()).await;
    assert!(saved.ok);
    for _ in 0..31 {
        assert!(matches!(
            control
                .store
                .reserve_runtime(
                    request(
                        &control.store.operation_epoch.current(),
                        Command::ShellWrite(ShellWriteArgs {
                            execution_id: "not-started".into(),
                            text: "never executed".into(),
                            encoding: "UTF-8".into()
                        })
                    ),
                    None
                )
                .await
                .unwrap(),
            RuntimeReservation::New(_)
        ));
    }
    for command in [
        Command::ThreadList(ThreadListArgs::default()),
        Command::StorageStatus(StorageArgs { category: None }),
    ] {
        let response = control
            .request(request(&control.store.operation_epoch.current(), command))
            .await;
        assert!(
            response.ok,
            "receipt-free observation remains available: {:?}",
            response.error
        );
    }
    let first_epoch = control.store.operation_epoch.current();
    let refused = control
        .request(request(
            &first_epoch,
            Command::StorageCollect(StorageArgs { category: None }),
        ))
        .await
        .error
        .unwrap();
    assert_eq!(refused.code, ErrorCode::StorageLimit);
    // The finished current-epoch identity moves the owner to the next epoch.
    assert_eq!(refused.details.unwrap()["reason"], "operation_epoch_rolled");
    assert_ne!(control.store.operation_epoch.current(), first_epoch);
    let lookup = control
        .request(request(
            &control.store.operation_epoch.current(),
            Command::OperationGet(OperationTarget {
                operation_id: saved_request.operation_id.clone(),
            }),
        ))
        .await;
    assert!(lookup.ok);
    assert_eq!(
        control
            .request(request(
                &control.store.operation_epoch.current(),
                Command::Status(StatusArgs::default())
            ))
            .await
            .result
            .unwrap()["receipt_budget"]["full_result_admission_blocked"],
        true
    );
    assert_eq!(
        control
            .request(saved_request.clone())
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::OperationExpired,
        "receipt pressure expires the terminal result without replaying its mutation"
    );
    control
        .gc
        .collect(StorageCategory::All, chrono::Utc::now())
        .await
        .unwrap();
    control.store.collect_receipts().await.unwrap();
    owner
        .db
        .call(|db| {
            // Pressure deletes the earlier-epoch identity; protected intent stays.
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r
                    .get::<_, i64>(0))?,
                31
            );
            Ok(())
        })
        .await
        .unwrap();
    let old_instance = control.store.instance_id.clone();
    control.shutdown().await.unwrap();
    drop(control);
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    let control = Control::new(&owner, root.path().into());
    assert_ne!(control.store.instance_id, old_instance);
    assert_eq!(control.store.collect_receipts().await.unwrap().collected, 0);
    let status = control
        .request(request(
            &control.store.operation_epoch.current(),
            Command::Status(StatusArgs::default()),
        ))
        .await;
    let status = status.result.unwrap();
    assert!(
        status["receipt_budget"]["protected_old_pending_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        status["receipt_budget"]["full_result_admission_blocked"],
        false
    );
    assert_eq!(
        control.request(saved_request).await.error.unwrap().code,
        ErrorCode::OperationExpired
    );
    assert!(
        control
            .request(request(
                &control.store.operation_epoch.current(),
                Command::ThreadList(ThreadListArgs {
                    root: None,
                    include_descendant_count: false,
                    include_activity: false,
                    include_detail: false,
                    thread_ids: None,
                    state: None,
                    order: Default::default(),
                    limit: 50,
                    cursor: None,
                    query: None,
                    awaiting: None,
                    archived: Default::default(),
                    tags: Vec::new(),
                    cwd: None,
                })
            ))
            .await
            .ok
    );
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM operation_receipt WHERE state='pending'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                31
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn current_instance_result_expiry_preserves_identity_conflicts_and_runtime_nonexecution() {
    let (_dir, store) = fixture().await;
    let create = request(
        "first.0",
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            parent_thread_id: None,
            cwd: None,
            connection: None,
            model: None,
            effort: None,
        }),
    );
    let created = store.execute(create.clone(), None).await.unwrap().result;
    let runtime = request(
        "first.0",
        Command::ShellWrite(ShellWriteArgs {
            execution_id: "synthetic".into(),
            text: "once".into(),
            encoding: "UTF-8".into(),
        }),
    );
    assert!(matches!(
        store.reserve_runtime(runtime.clone(), None).await.unwrap(),
        RuntimeReservation::New(_)
    ));
    store
        .finalize_runtime(runtime.operation_id.clone(), Ok(json!({"accepted":true})))
        .await
        .unwrap();
    store.database.call(|db| {db.execute("UPDATE operation_receipt SET created_at='2000-01-01' WHERE state IN ('completed','failed')",[])?;Ok(())}).await.unwrap();
    assert_eq!(store.collect_receipts().await.unwrap().collected, 2);
    assert_eq!(
        store.execute(create.clone(), None).await.unwrap_err().code,
        ErrorCode::OperationExpired
    );
    assert_eq!(
        store.replay(&create, &None).await.unwrap_err().code,
        ErrorCode::OperationExpired
    );
    let mut changed = create.clone();
    if let Command::ThreadCreate(args) = &mut changed.command {
        args.cwd = Some("different-cwd".into());
    }
    assert_eq!(
        store.execute(changed, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    assert!(matches!(
        store.reserve_runtime(runtime.clone(), None).await.unwrap(),
        RuntimeReservation::Saved(Err(ApiError {
            code: ErrorCode::OperationExpired,
            ..
        }))
    ));
    let mut changed = runtime.clone();
    if let Command::ShellWrite(args) = &mut changed.command {
        args.text = "twice".into();
    }
    assert_eq!(
        store.reserve_runtime(changed, None).await.unwrap_err().code,
        ErrorCode::OperationConflict
    );
    let details = inspect(&store, &create.operation_id).await;
    assert_eq!(details["state"], "completed");
    assert_eq!(details["result_expired"], true);
    assert_eq!(details["replay_policy"], "operation_expired");
    assert_eq!(details["outcome"]["error"]["code"], "operation_expired");
    assert_eq!(details["result_uncertain"], false);
    let status = store
        .execute(
            request(
                "first.0",
                Command::OperationGet(OperationTarget {
                    operation_id: runtime.operation_id.clone(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(status["result_expired"], true);
    assert_eq!(status["replay_policy"], "operation_expired");
    assert_eq!(
        store
            .execute(
                request("first.0", Command::Status(StatusArgs::default())),
                None
            )
            .await
            .unwrap()
            .result["receipt_budget"]["result_retention_seconds"],
        86_400
    );
    assert_eq!(store.collect_receipts().await.unwrap().collected, 0);
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM shell_execution", [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT result_ref_json FROM operation_receipt WHERE operation_id=?1",
                    [create.operation_id],
                    |row| row.get::<_, String>(0)
                )?,
                "0"
            );
            assert!(created["thread_id"].as_str().is_some());
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

const FILL_FINISHED: &str = "INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes,result_ref_json)VALUES(?1,?2,'\"'||hex(zeroblob(4194304))||'\"','hash','completed',?3,0,'0')";

fn create_thread() -> Command {
    Command::ThreadCreate(ThreadCreateArgs {
        agents: None,
        parent_thread_id: None,
        cwd: None,
        connection: None,
        model: None,
        effort: None,
    })
}

/// An owner served over its real IPC socket whose receipt budget is full of
/// finished current-epoch identities.
async fn full_owner() -> (
    tempfile::TempDir,
    DataOwner,
    shellboy::ipc::LocalServer,
    tokio::task::JoinHandle<()>,
) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = shellboy::control::Control::new(&owner, root.path().into());
    control.start().await.unwrap();
    let instance = owner.instance_id.clone();
    owner
        .db
        .call(move |db| {
            for n in 1..=31 {
                db.execute(
                    FILL_FINISHED,
                    rusqlite::params![
                        format!("{instance}.0:filler-{n}"),
                        instance,
                        chrono::Utc::now().to_rfc3339()
                    ],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let (send, receive) = tokio::sync::mpsc::channel(8);
    let server = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(async move {
        let _ = control.serve(receive).await;
    });
    (root, owner, server, serving)
}

async fn thread_count(data: &std::path::Path) -> usize {
    let response = shellboy::ipc::call(
        data,
        Command::ThreadList(ThreadListArgs {
            limit: 100,
            ..Default::default()
        }),
        None,
        None,
    )
    .await
    .unwrap();
    response.result.unwrap()["items"].as_array().unwrap().len()
}

#[tokio::test]
async fn client_mutation_after_an_epoch_roll_mints_in_the_new_epoch_and_applies_once() {
    let (_root, owner, server, serving) = full_owner().await;
    let data = owner.loaded.data_dir.clone();
    let mut announced = Vec::new();
    let refused = shellboy::ipc::call_announcing(&data, create_thread(), None, &mut |id| {
        announced.push(id.to_owned())
    })
    .await
    .unwrap();
    assert_eq!(announced, std::slice::from_ref(&refused.operation_id));
    let error = refused.error.unwrap();
    assert_eq!(error.code, ErrorCode::StorageLimit);
    let details = error.details.unwrap();
    assert_eq!(details["reason"], "operation_epoch_rolled");
    assert_eq!(details["request_may_have_been_applied"], false);
    assert_eq!(thread_count(&data).await, 0);
    // The rolled request's ID is outside the new epoch and never runs.
    let reused = shellboy::ipc::call(&data, create_thread(), Some(refused.operation_id), None)
        .await
        .unwrap();
    assert_eq!(reused.error.unwrap().code, ErrorCode::OperationExpired);
    assert_eq!(thread_count(&data).await, 0);
    // A fresh request without a supplied ID is minted in the new epoch.
    let mut fresh = Vec::new();
    let created = shellboy::ipc::call_announcing(&data, create_thread(), None, &mut |id| {
        fresh.push(id.to_owned())
    })
    .await
    .unwrap();
    assert!(created.ok, "{:?}", created.error);
    assert_ne!(fresh, announced);
    assert_eq!(thread_count(&data).await, 1);
    server.shutdown().await.unwrap();
    serving.abort();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn pressure_expiry_commits_on_refusal_without_reexecuting_or_deleting_current_identity() {
    let (_dir, store) = fixture().await;
    let original = request(
        "first.0",
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            parent_thread_id: None,
            cwd: None,
            connection: None,
            model: None,
            effort: None,
        }),
    );
    store.execute(original.clone(), None).await.unwrap();
    store.database.call(|db| {
        db.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,created_at,reserved_bytes)VALUES('first:pressure','first','null','hash','pending','2000-01-01',268435456)",[])?;
        Ok(())
    }).await.unwrap();
    let rejected = request(
        "first.0",
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            parent_thread_id: None,
            cwd: None,
            connection: None,
            model: None,
            effort: None,
        }),
    );
    assert_eq!(
        store
            .execute(rejected.clone(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageLimit
    );
    assert_eq!(
        store
            .execute(original.clone(), None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::OperationExpired
    );
    let status = store
        .execute(
            request(
                &store.operation_epoch.current(),
                Command::OperationGet(OperationTarget {
                    operation_id: original.operation_id.clone(),
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(status["result_expired"], true);
    // Stops retain the existing finite headroom even when ordinary admission
    // remains blocked by protected pending reservations.
    // The refusal rolled the epoch; new work uses the current one.
    assert_eq!(store.operation_epoch.current(), "first.1");
    let stop = request(
        &store.operation_epoch.current(),
        Command::ShellStop(ExecutionTarget {
            execution_id: "synthetic".into(),
        }),
    );
    assert!(matches!(
        store.reserve_runtime(stop.clone(), None).await.unwrap(),
        RuntimeReservation::New(_)
    ));
    store
        .finalize_runtime(stop.operation_id.clone(), Ok(json!({"accepted":true})))
        .await
        .unwrap();
    assert!(matches!(
        store.reserve_runtime(stop, None).await.unwrap(),
        RuntimeReservation::Saved(Ok(_))
    ));
    store
        .database
        .call(move |db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id=?1",
                    [original.operation_id],
                    |row| row.get::<_, i64>(0)
                )?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id=?1",
                    [rejected.operation_id],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM thread", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT state FROM operation_receipt WHERE operation_id='first:pressure'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "pending"
            );
            Ok(())
        })
        .await
        .unwrap();
    store.database.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_middle_omit_and_restore_apply_once_with_durable_downstream_results() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = shellboy::control::Control::new(&owner, root.path().into());
    let created = control
        .request(request(
            &owner.operation_epoch.current(),
            Command::ThreadCreate(ThreadCreateArgs {
                agents: None,
                cwd: None,
                connection: None,
                model: None,
                effort: None,
                parent_thread_id: None,
            }),
        ))
        .await;
    assert!(created.ok, "{created:?}");
    let thread = created.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let target = thread.clone();
    owner
        .db
        .call(move |db| {
            let tx = db.transaction()?;
            for id in ["before", "omit-a", "omit-b", "after"] {
                shellboy::history::append_node(
                    &tx,
                    &target,
                    id,
                    None,
                    "user_message",
                    &json!({"role":"user","content":id}),
                    "now",
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
    for action in [ContextEditAction::Omit, ContextEditAction::Restore] {
        let omit = action == ContextEditAction::Omit;
        let req = request(
            &owner.operation_epoch.current(),
            Command::ContextEdit(ContextEditArgs {
                max_chars: None,
                thread_id: Some(thread.clone()),
                action,
                from_node_id: omit.then(|| "omit-a".into()),
                to_node_id: omit.then(|| "omit-b".into()),
            }),
        );
        let accepted = control.request(req.clone()).await;
        assert!(accepted.ok, "{accepted:?}");
        assert_eq!(accepted.result.as_ref().unwrap()["status"], "queued");
        let outcome = crate::test_watchdog::watchdog("tests/operation_contract.rs: queued_middle_omit_and_restore_apply_once_with_durable_downstream_results", async {
            loop {
                let status = control
                    .request(request(
                        &owner.operation_epoch.current(),
                        Command::OperationGet(OperationTarget {
                            operation_id: req.operation_id.clone(),
                        }),
                    ))
                    .await;
                assert!(status.ok, "{status:?}");
                let downstream = status.result.unwrap()["downstream"].clone();
                match downstream["state"].as_str() {
                    Some("completed") => break downstream["outcome"]["result"].clone(),
                    Some("failed") => panic!("context edit failed: {downstream}"),
                    _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                }
            }
        })
        .await
        .expect("queued context edit did not finish");
        assert!(outcome["after_characters"].is_u64());
        assert!(outcome["changed_suffix_characters"].is_u64());
        let replay = control.request(req).await;
        assert_eq!(
            replay.result, accepted.result,
            "replay returns immutable acceptance, not a second edit"
        );
        let selected_thread = thread.clone();
        owner
            .db
            .call(move |db| {
                let path = db
                    .prepare("SELECT node_id FROM thread_path WHERE thread_id=?1 ORDER BY ordinal")?
                    .query_map([selected_thread], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if omit {
                    assert_eq!(path.len(), 3);
                    assert_eq!(path[0], "before");
                    assert_eq!(path[2], "after");
                } else {
                    assert_eq!(path, ["before", "omit-a", "omit-b", "after"]);
                }
                assert_eq!(
                    db.query_row("SELECT count(*) FROM replacement", [], |row| row
                        .get::<_, i64>(0))?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
