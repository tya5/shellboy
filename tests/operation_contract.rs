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
                &store.instance_id,
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
                &store.instance_id,
                Command::ThreadCreate(ThreadCreateArgs {
                    parent_id: None,
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
async fn lost_response_replays_original_acceptance_after_message_progress_and_restart() {
    let (_dir, store) = fixture().await;
    let thread = thread(&store).await;
    let req = request(
        "first",
        Command::MessageSend(MessageSendArgs {
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
        "first",
        Command::ShellWrite(ShellWriteArgs {
            execution_id: "synthetic".into(),
            text: "write once".into(),
            encoding: "UTF-8".into(),
        }),
    );
    assert!(matches!(
        store.reserve_runtime(req.clone(), None).await.unwrap(),
        RuntimeReservation::New
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
        &store.instance_id,
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
        &owner.instance_id,
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
            &owner.instance_id,
            Command::ThreadDelete(ThreadDeleteArgs {
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
    let rejected = control
        .request(request(
            &control.store.instance_id,
            Command::StorageCollect(StorageArgs { category: None }),
        ))
        .await;
    assert_eq!(rejected.error.unwrap().code, ErrorCode::StorageLimit);
    let lookup = control
        .request(request(
            &control.store.instance_id,
            Command::Status(StatusArgs {
                operation_id: Some(pending.clone()),
                ..Default::default()
            }),
        ))
        .await;
    assert!(
        lookup.ok,
        "Capacity rejection must not enter save-failed shutdown: {:?}",
        lookup.error
    );
    assert_eq!(lookup.result.unwrap()["operation"]["state"], "pending");
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
            &control.store.instance_id,
            Command::ThreadList(ThreadListArgs {
                limit: 50,
                cursor: None,
                query: None,
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
#[ignore = "manual operation-size sample; temporary owner, no model API"]
async fn measure_current_instance_receipt_sizes() {
    use rusqlite::OptionalExtension;
    use shellboy::control::Control;
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
    let control = Control::new(&owner, root.path().into());
    let id = thread(&control.store).await;
    let id_copy = id.clone();
    owner.db.call(move|db| {
        let tx=db.transaction()?;
        shellboy::history::append_node(&tx,&id_copy,"body",None,"message",&json!({"role":"user","content":"a".repeat(65536)}),"2026-10-25T00:00:00Z")?;
        tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('run',?1,'completed','now')",[&id_copy])?;
        tx.execute_batch("INSERT INTO turn(id,thread_id,run_id,state,created_at) SELECT 'turn',thread_id,'run','completed','now' FROM run WHERE id='run'; INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES('shell','run','turn','shell-op','exited','/tmp','{}','UTF-8','UTF-8','closed',1,0,1,'now','now');")?;
        tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('shell',0,'stdout',0,?1,'now')",[vec![b'a';65536]])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    let commands = vec![
        (
            "thread.get (one thread)",
            Command::ThreadGet(ThreadTarget {
                thread_id: Some(id.clone()),
            }),
        ),
        (
            "thread.list (one thread)",
            Command::ThreadList(ThreadListArgs {
                limit: 50,
                cursor: None,
                query: None,
            }),
        ),
        (
            "history.list (one 64KiB node)",
            Command::HistoryList(HistoryListArgs {
                thread_id: id,
                limit: 50,
                cursor: None,
                run_id: None,
                since: None,
                state: None,
            }),
        ),
        (
            "history.get (64KiB page)",
            Command::HistoryGet(HistoryGetArgs {
                node_id: "body".into(),
                limit_bytes: 65536,
                cursor: None,
                encoding: "UTF-8".into(),
            }),
        ),
        (
            "shell.get (exited)",
            Command::ShellGet(ExecutionTarget {
                execution_id: "shell".into(),
            }),
        ),
        (
            "shell.read (64KiB page)",
            Command::ShellRead(ShellReadArgs {
                execution_id: "shell".into(),
                cursor: None,
                encoding: Some("UTF-8".into()),
                limit_bytes: 65536,
            }),
        ),
        (
            "wait (already-ended shell)",
            Command::Wait(WaitArgs {
                conditions: vec![WaitCondition::ShellEnded {
                    execution_id: "shell".into(),
                }],
                timeout_ms: 1,
            }),
        ),
        (
            "storage.status (fixture)",
            Command::StorageStatus(StorageArgs { category: None }),
        ),
    ];
    for (name, command) in commands {
        let mut sizes = Vec::new();
        let mut durations = Vec::new();
        for _ in 0..10 {
            let request = request(&control.store.instance_id, command.clone());
            let operation = request.operation_id.clone();
            let started = std::time::Instant::now();
            let result = control.request(request).await;
            assert!(result.ok, "{name}: {:?}", result.error);
            durations.push(started.elapsed());
            sizes.push(owner.db.call(move|db|Ok(db.query_row("SELECT 512+octet_length(operation_id)+octet_length(instance_id)+octet_length(origin_json)+octet_length(request_hash)+octet_length(created_at)+COALESCE(octet_length(result_ref_json),0) FROM operation_receipt WHERE operation_id=?1",[operation],|r|r.get::<_,i64>(0)).optional()?.unwrap_or(0))).await.unwrap());
        }
        sizes.sort_unstable();
        durations.sort_unstable();
        let bytes = sizes[5];
        println!(
            "{name}: bytes={}..{} median={}, projected_admissions={} p50_ms={:.3}",
            sizes[0],
            sizes[9],
            bytes,
            if bytes == 0 {
                "receipt-free".to_owned()
            } else {
                ((256 * 1024 * 1024 - 8 * 1024 * 1024 - 1024) / bytes).to_string()
            },
            durations[5].as_secs_f64() * 1000.0
        );
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn full_receipt_budget_keeps_replay_status_gc_and_explicit_owner_restart_available() {
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
        &control.store.instance_id,
        Command::ThreadCreate(ThreadCreateArgs {
            parent_id: None,
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
                        &control.store.instance_id,
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
            RuntimeReservation::New
        ));
    }
    for command in [
        Command::ThreadList(ThreadListArgs::default()),
        Command::StorageStatus(StorageArgs { category: None }),
    ] {
        let response = control
            .request(request(&control.store.instance_id, command))
            .await;
        assert!(
            response.ok,
            "receipt-free observation remains available: {:?}",
            response.error
        );
    }
    assert_eq!(
        control
            .request(request(
                &control.store.instance_id,
                Command::StorageCollect(StorageArgs { category: None })
            ))
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::StorageLimit
    );
    let lookup = control
        .request(request(
            &control.store.instance_id,
            Command::Status(StatusArgs {
                operation_id: Some(saved_request.operation_id.clone()),
                ..Default::default()
            }),
        ))
        .await;
    assert!(lookup.ok);
    assert_eq!(
        lookup.result.unwrap()["receipt_budget"]["full_result_admission_blocked"],
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
    assert_eq!(control.store.collect_receipts().await.unwrap(), 0);
    owner
        .db
        .call(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r
                    .get::<_, i64>(0))?,
                32
            );
            // Make the terminal fixture old enough for ordinary post-restart GC.
            db.execute(
                "UPDATE operation_receipt SET created_at='2000-01-01' WHERE state='completed'",
                [],
            )?;
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
    assert_eq!(control.store.collect_receipts().await.unwrap(), 1);
    let status = control
        .request(request(
            &control.store.instance_id,
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
                &control.store.instance_id,
                Command::ThreadList(ThreadListArgs {
                    limit: 50,
                    cursor: None,
                    query: None
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
        "first",
        Command::ThreadCreate(ThreadCreateArgs {
            parent_id: None,
            cwd: None,
            connection: None,
            model: None,
            effort: None,
        }),
    );
    let created = store.execute(create.clone(), None).await.unwrap().result;
    let runtime = request(
        "first",
        Command::ShellWrite(ShellWriteArgs {
            execution_id: "synthetic".into(),
            text: "once".into(),
            encoding: "UTF-8".into(),
        }),
    );
    assert!(matches!(
        store.reserve_runtime(runtime.clone(), None).await.unwrap(),
        RuntimeReservation::New
    ));
    store
        .finalize_runtime(runtime.operation_id.clone(), Ok(json!({"accepted":true})))
        .await
        .unwrap();
    store.database.call(|db| {db.execute("UPDATE operation_receipt SET created_at='2000-01-01' WHERE state IN ('completed','failed')",[])?;Ok(())}).await.unwrap();
    assert_eq!(store.collect_receipts().await.unwrap(), 2);
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
                "first",
                Command::Status(StatusArgs {
                    operation_id: Some(runtime.operation_id.clone()),
                    ..Default::default()
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(status["operation"]["result_expired"], true);
    assert_eq!(status["operation"]["replay_policy"], "operation_expired");
    assert_eq!(status["receipt_budget"]["result_retention_seconds"], 86_400);
    assert_eq!(store.collect_receipts().await.unwrap(), 0);
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

#[tokio::test]
async fn pressure_expiry_commits_on_refusal_without_reexecuting_or_deleting_current_identity() {
    let (_dir, store) = fixture().await;
    let original = request(
        "first",
        Command::ThreadCreate(ThreadCreateArgs {
            parent_id: None,
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
        "first",
        Command::ThreadCreate(ThreadCreateArgs {
            parent_id: None,
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
                "first",
                Command::Status(StatusArgs {
                    operation_id: Some(original.operation_id.clone()),
                    ..Default::default()
                }),
            ),
            None,
        )
        .await
        .unwrap()
        .result;
    assert_eq!(status["operation"]["result_expired"], true);
    // Stops retain the existing finite headroom even when ordinary admission
    // remains blocked by protected pending reservations.
    let stop = request(
        "first",
        Command::ShellStop(ExecutionTarget {
            execution_id: "synthetic".into(),
        }),
    );
    assert!(matches!(
        store.reserve_runtime(stop.clone(), None).await.unwrap(),
        RuntimeReservation::New
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
