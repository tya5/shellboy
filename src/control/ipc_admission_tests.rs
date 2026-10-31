//! The real dispatcher must retain work admission after its observer disappears.
use super::*;
use tokio::sync::{Semaphore, oneshot};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_ipc_waiters_leave_status_and_run_stop_available() {
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
    owner.db.call(|db| {
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('thread','/tmp','/tmp/AGENTS.md','fixture','fixture','2026-01-01T00:00:00Z'); INSERT INTO run(id,thread_id,state,created_at) VALUES('run','thread','live','2026-01-01T00:00:00Z');")?;
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (send, receive) = mpsc::channel(32);
    let server = crate::ipc::LocalServer::bind(&owner, send).await.unwrap();
    let dispatcher = tokio::spawn(control.clone().serve(receive));
    let mut clients = Vec::new();
    for number in 0..24 {
        let data = owner.loaded.data_dir.clone();
        let operation = format!("{}:wait-{number}", owner.instance_id);
        clients.push(tokio::spawn(async move {
            crate::ipc::call(
                &data,
                Command::Wait(WaitArgs {
                    after: None,
                    conditions: vec![WaitCondition::RunEnded {
                        run_id: "run".into(),
                    }],
                    timeout_ms: 600000,
                }),
                Some(operation),
                None,
            )
            .await
        }));
    }
    crate::test_watchdog::watchdog(
        "src/control/ipc_admission_tests.rs: real_ipc_waiters_leave_status_and_run_stop_available",
        async {
            loop {
                let count = owner
                    .db
                    .call(|db| {
                        Ok(db.query_row(
                            "SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                            [],
                            |r| r.get::<_, i64>(0),
                        )?)
                    })
                    .await
                    .unwrap();
                if count == 24 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
    let status = crate::test_watchdog::watchdog(
        "src/control/ipc_admission_tests.rs: real_ipc_waiters_leave_status_and_run_stop_available",
        crate::ipc::call(
            &owner.loaded.data_dir,
            Command::Status(StatusArgs::default()),
            None,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(status.ok, "{status:?}");
    let still_pending = owner
        .db
        .call(|db| {
            Ok(db.query_row(
                "SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%'",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        still_pending, 24,
        "status must bypass live waiters, not wait for their expiry"
    );
    let stopped = crate::test_watchdog::watchdog(
        "src/control/ipc_admission_tests.rs: real_ipc_waiters_leave_status_and_run_stop_available",
        crate::ipc::call(
            &owner.loaded.data_dir,
            Command::RunStop(RunTarget {
                run_id: "run".into(),
            }),
            None,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(stopped.ok, "{stopped:?}");
    for client in clients {
        assert!(
            crate::test_watchdog::watchdog("src/control/ipc_admission_tests.rs: real_ipc_waiters_leave_status_and_run_stop_available", client)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .ok
        );
    }
    let state = owner
        .db
        .call(|db| {
            Ok(
                db.query_row("SELECT state FROM run WHERE id='run'", [], |row| {
                    row.get::<_, String>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(state, "stopped");
    server.shutdown().await.unwrap();
    dispatcher.await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn disconnected_ipc_mutations_retain_all_32_permits_until_evaluation_completes() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        r#"default_connection="fixture"
default_model="fixture"
[connections.fixture]
base_url="http://127.0.0.1:1/v1"
"#,
    )
    .unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().to_path_buf(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().to_path_buf());
    let (entered, ready) = oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let gate = owner
        .db
        .try_call(move |_| {
            let _ = entered.send(());
            blocked.recv().unwrap();
            Ok(())
        })
        .unwrap();
    ready.await.unwrap();
    let slots = Arc::new(Semaphore::new(32));
    let (send, receive) = mpsc::channel(32);
    let dispatcher = tokio::spawn(control.clone().serve(receive));
    for number in 0..32 {
        let (reply, observer) = oneshot::channel();
        drop(observer);
        let disconnected = CancellationToken::new();
        disconnected.cancel();
        send.send(Invocation {
            request: Request::new(
                format!("{}:{number}", owner.instance_id),
                Command::ThreadCreate(ThreadCreateArgs {
                    agents: None,
                    connection: None,
                    cwd: None,
                    model: None,
                    effort: None,
                    parent_id: None,
                }),
            ),
            reply,
            disconnected,
            admission: slots.clone().acquire_owned().await.unwrap(),
        })
        .await
        .unwrap();
    }
    drop(send);
    dispatcher.await.unwrap();
    crate::test_watchdog::watchdog("src/control/ipc_admission_tests.rs: disconnected_ipc_mutations_retain_all_32_permits_until_evaluation_completes", async {
        while control.active_requests.load(Ordering::Acquire) != 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(slots.available_permits(), 0);
    let next = slots.clone().acquire_owned();
    tokio::pin!(next);
    assert!(
        futures_util::poll!(&mut next).is_pending(),
        "a disconnected accepted mutation still owns its slot"
    );
    release.send(()).unwrap();
    gate.receive().await.unwrap();
    let permit = crate::test_watchdog::watchdog("src/control/ipc_admission_tests.rs: disconnected_ipc_mutations_retain_all_32_permits_until_evaluation_completes", next)
        .await
        .unwrap()
        .unwrap();
    drop(permit);
    control.shutdown().await.unwrap();
    owner
        .db
        .call(|connection| {
            let count: i64 =
                connection.query_row("SELECT COUNT(*) FROM thread", [], |row| row.get(0))?;
            assert_eq!(
                count, 32,
                "disconnection must not cancel accepted mutations"
            );
            Ok(())
        })
        .await
        .unwrap();
    owner.shutdown().await.unwrap();
}

// This fixture needs private admission facts, not a sleep or a public test hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnecting_a_send_observer_does_not_lose_commit_effects_or_race_shutdown() {
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
    let response = control
        .request(Request::new(
            format!("{}:create", control.store.instance_id),
            Command::ThreadCreate(crate::protocol::ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            }),
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    let thread = response.result.unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (entered, ready) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let db = control.store.database.clone();
    let blocker = tokio::spawn(async move {
        db.call(move |_| {
            entered.send(()).unwrap();
            crate::test_watchdog::blocking_recv("release cancelled-observer DB barrier", &gate)
                .unwrap();
            Ok(())
        })
        .await
        .unwrap()
    });
    ready.await.unwrap();
    let operation = format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4());
    let op = operation.clone();
    let sender = control.clone();
    let observer = tokio::spawn(async move {
        sender
            .request(Request::new(
                op,
                Command::MessageSend(crate::protocol::MessageSendArgs {
                    sender_label: None,
                    now: false,
                    thread_id: Some(thread),
                    text: "accepted despite observer cancellation".into(),
                }),
            ))
            .await
    });
    crate::test_watchdog::watchdog(
        "owned worker admitted before observation is cancelled",
        async {
            while control.active_requests.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
    observer.abort();
    let stopping = control.clone();
    let shutdown = tokio::spawn(async move { stopping.shutdown().await });
    crate::test_watchdog::watchdog("shutdown closes admission before blocked commit", async {
        while !control.admission_closed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !shutdown.is_finished(),
        "DB barrier must keep actual shutdown pending"
    );
    release.send(()).unwrap();
    crate::test_watchdog::watchdog("DB barrier completes", blocker)
        .await
        .unwrap()
        .unwrap();
    crate::test_watchdog::watchdog("cancelled observer shutdown completes", shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let result = control
        .store
        .database
        .call(move |db| {
            Ok((
                db.query_row(
                    "SELECT count(*) FROM operation_receipt WHERE operation_id=?1",
                    [operation],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM run WHERE state IN ('live','stopping')",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM turn WHERE state IN ('preparing','running','waiting')",
                    [],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(result, (1, 0, 0));
    owner.shutdown().await.unwrap();
}
