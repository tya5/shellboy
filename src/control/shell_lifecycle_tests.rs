use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    time::{Duration, timeout},
};

pub(super) type ModelRequest = (Value, oneshot::Sender<Value>);
pub(super) type Fixture = (
    tempfile::TempDir,
    DataOwner,
    Arc<Control>,
    mpsc::UnboundedReceiver<ModelRequest>,
    tokio::task::JoinHandle<()>,
);
pub(super) async fn fixture() -> Fixture {
    fixture_with_settings("").await
}
pub(super) async fn fixture_with_settings(extra: &str) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::unbounded_channel();
    let serving = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let send = send.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let split = loop {
                    let mut part = [0; 4096];
                    let count = stream.read(&mut part).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&part[..count]);
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                };
                let length = String::from_utf8_lossy(&bytes[..split])
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while bytes.len() < split + length {
                    let mut part = [0; 4096];
                    let count = stream.read(&mut part).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&part[..count]);
                }
                let (reply, waiting) = oneshot::channel::<Value>();
                send.send((
                    serde_json::from_slice(&bytes[split..split + length]).unwrap(),
                    reply,
                ))
                .unwrap();
                if let Ok(value) = waiting.await {
                    let body = value.to_string();
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if stream.write_all(header.as_bytes()).await.is_ok() {
                        let _ = stream.write_all(body.as_bytes()).await;
                    }
                }
            });
        }
    });
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\n[io]\nterm_grace_ms=50\ndrain_timeout_ms=100\nself_reply_grace_ms=60000\n{extra}")).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    (root, owner, control, receive, serving)
}
pub(super) async fn rpc(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
pub(super) async fn start(control: &Arc<Control>) -> String {
    let thread = rpc(
        control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            model: None,
            effort: None,
            cwd: None,
            parent_id: None,
        }),
    )
    .await;
    let thread = thread["thread_id"].as_str().unwrap().to_owned();
    rpc(
        control,
        Command::MessageSend(MessageSendArgs {
            now: false,
            thread_id: Some(thread.clone()),
            text: "shell lifecycle fixture".into(),
        }),
    )
    .await;
    thread
}
pub(super) async fn incoming(requests: &mut mpsc::UnboundedReceiver<ModelRequest>) -> ModelRequest {
    timeout(Duration::from_secs(5), requests.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn starting(control: &Arc<Control>) -> String {
    timeout(Duration::from_secs(3), async {
        loop {
            let row = control
                .store
                .database
                .call(|db| {
                    use rusqlite::OptionalExtension;
                    Ok(db
                        .query_row(
                            "SELECT id FROM shell_execution WHERE state='starting'",
                            [],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()?)
                })
                .await
                .unwrap();
            if let Some(id) = row {
                return id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
pub(super) fn tool(command: &str) -> Value {
    json!({"id":"lifecycle-response","status":"completed","output":[{"type":"function_call","call_id":"lifecycle-call","name":"shell","arguments":json!({"command":command,"stdin_open":true,"initial_stdin":"INITIAL"}).to_string()}]})
}
pub(super) fn answer() -> Value {
    json!({"id":"finished","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn starting_stop_and_eof_survive_the_real_driver_pre_spawn_boundary() {
    for eof in [false, true] {
        let (root, owner, control, mut requests, server) = fixture().await;
        // Origin publication occurs after starting commit and before spawn.
        // Holding that existing mutex is an acknowledged real boundary.
        let origin_gate = control.origins.lock().await;
        start(&control).await;
        let (_, reply) = incoming(&mut requests).await;
        let marker = root.path().join("must-not-spawn");
        let command = if eof {
            "cat; printf EOF_SEEN".to_owned()
        } else {
            format!("printf STARTED > '{}'; sleep 60", marker.display())
        };
        reply.send(tool(&command)).unwrap();
        let execution = starting(&control).await;
        let result = rpc(
            &control,
            if eof {
                Command::ShellEof(ExecutionTarget {
                    execution_id: execution.clone(),
                })
            } else {
                Command::ShellStop(ExecutionTarget {
                    execution_id: execution.clone(),
                })
            },
        )
        .await;
        assert_eq!(result["accepted"], true);
        let observed = execution.clone();
        control
            .store
            .database
            .call(move |db| {
                let (state, stdin): (String, String) = db.query_row(
                    "SELECT state,stdin_state FROM shell_execution WHERE id=?1",
                    [observed],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if eof {
                    assert_eq!(stdin, "closed");
                } else {
                    assert_eq!(state, "stopping");
                }
                Ok(())
            })
            .await
            .unwrap();
        drop(origin_gate);
        let (next, reply) = incoming(&mut requests).await;
        if eof {
            assert!(next.to_string().contains("INITIALEOF_SEEN"));
        } else {
            assert!(!marker.exists());
            assert!(next.to_string().contains("stopped_before_spawn"));
        }
        reply.send(answer()).unwrap();
        control.shutdown().await.unwrap();
        let execution_copy = execution.clone();
        control.store.database.call(move|db| {
            let (spawned,state):(Option<bool>,String)=db.query_row("SELECT json_extract(shell_json,'$.spawned'),state FROM shell_execution WHERE id=?1",[execution_copy],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(spawned.unwrap_or(false),eof);assert!(!matches!(state.as_str(),"starting"|"running"|"stopping"));Ok(())
        }).await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }
}

async fn wait_saved_signal(control: &Arc<Control>, execution: &str, eof: bool) {
    let execution = execution.to_owned();
    timeout(Duration::from_secs(3), async {
        loop {
            let execution = execution.clone();
            let saved = control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        if eof {
                            "SELECT stdin_state='closed' FROM shell_execution WHERE id=?1"
                        } else {
                            "SELECT state='stopping' FROM shell_execution WHERE id=?1"
                        },
                        [execution],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap();
            if saved {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn wait_marker(path: &std::path::Path) {
    timeout(Duration::from_secs(3), async {
        while tokio::fs::metadata(path).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_and_eof_during_real_spawn_registration_are_reapplied() {
    for eof in [false, true] {
        let (root, owner, control, mut requests, server) = fixture().await;
        let origin_gate = control.origins.lock().await;
        start(&control).await;
        let (_, reply) = incoming(&mut requests).await;
        let marker = root.path().join("spawned");
        reply
            .send(tool(&format!(
                "printf READY > '{}'; cat; printf EOF_SEEN",
                marker.display()
            )))
            .unwrap();
        let execution = starting(&control).await;
        let registration_gate = control.live.test_shells().lock().await;
        drop(origin_gate);
        wait_marker(&marker).await; // Real OS side effect before handle publication.
        let signaling = control.clone();
        let target = execution.clone();
        let signal = tokio::spawn(async move {
            rpc(
                &signaling,
                if eof {
                    Command::ShellEof(ExecutionTarget {
                        execution_id: target,
                    })
                } else {
                    Command::ShellStop(ExecutionTarget {
                        execution_id: target,
                    })
                },
            )
            .await
        });
        wait_saved_signal(&control, &execution, eof).await;
        drop(registration_gate);
        assert_eq!(signal.await.unwrap()["accepted"], true);
        let (next, reply) = incoming(&mut requests).await;
        if eof {
            assert!(next.to_string().contains("INITIALEOF_SEEN"));
        }
        reply.send(answer()).unwrap();
        control.shutdown().await.unwrap();
        control.store.database.call(move|db| {
            let (spawned,state):(bool,String)=db.query_row("SELECT json_extract(shell_json,'$.spawned'),state FROM shell_execution WHERE id=?1",[execution],|r|Ok((r.get(0)?,r.get(1)?)))?;
            assert!(spawned);assert!(!matches!(state.as_str(),"starting"|"running"|"stopping"));Ok(())
        }).await.unwrap();
        owner.shutdown().await.unwrap();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn self_stop_during_registration_keeps_reply_grace_owned_until_shutdown_drain() {
    let (root, owner, control, mut requests, server) = fixture().await;
    let origin_gate = control.origins.lock().await;
    start(&control).await;
    let (_, reply) = incoming(&mut requests).await;
    let marker = root.path().join("self-stop-spawned");
    reply
        .send(tool(&format!("printf READY > '{}'; cat", marker.display())))
        .unwrap();
    let execution = starting(&control).await;
    let registration_gate = control.live.test_shells().lock().await;
    drop(origin_gate);
    wait_marker(&marker).await;
    let origin = control
        .origins
        .lock()
        .await
        .values()
        .find(|o| o.execution_id == execution)
        .unwrap()
        .clone();
    let operation = format!("{}:self-stop-registration", control.store.instance_id);
    let mut request = Request::new(
        operation.clone(),
        Command::ShellStop(ExecutionTarget {
            execution_id: execution.clone(),
        }),
    );
    request.origin = Some(origin);
    let sending = control.clone();
    let signal = tokio::spawn(async move { sending.request(request).await });
    wait_saved_signal(&control, &execution, false).await;
    drop(registration_gate);
    let response = timeout(Duration::from_secs(3), signal)
        .await
        .unwrap()
        .unwrap();
    assert!(response.ok, "{:?}", response.error);
    let shell = control
        .live
        .test_shells()
        .lock()
        .await
        .get(&execution)
        .unwrap()
        .clone();
    assert_eq!(
        shell
            .handle
            .write("AFTER_ACCEPTED", "UTF-8")
            .await
            .unwrap()
            .status,
        crate::shell::StdinWriteStatus::Complete
    );
    assert!(
        control
            .runtime_operations
            .lock()
            .await
            .contains_key(&operation),
        "saved reply must not release its grace owner"
    );
    let started = tokio::time::Instant::now();
    timeout(Duration::from_secs(3), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(control.runtime_operations.lock().await.is_empty());
    assert!(control.live.test_shells().lock().await.is_empty());
    assert_eq!(control.active_requests.load(Ordering::Acquire), 0);
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_stop_reply_grace_remains_in_the_request_guard_and_freeze_releases_it() {
    let (_root, owner, control, mut requests, server) = fixture().await;
    let thread = start(&control).await;
    let _pending = incoming(&mut requests).await;
    let target = thread.clone();
    let run = control
        .store
        .database
        .call(move |db| {
            Ok(db.query_row(
                "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
                [target],
                |r| r.get::<_, String>(0),
            )?)
        })
        .await
        .unwrap();
    let stopping = control.clone();
    let target = run.clone();
    let request = tokio::spawn(async move {
        rpc(&stopping, Command::RunStop(RunTarget { run_id: target })).await
    });
    timeout(Duration::from_secs(3), async {
        loop {
            let target = run.clone();
            if control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT state='stopping' FROM run WHERE id=?1",
                        [target],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    timeout(Duration::from_secs(3), control.shutdown())
        .await
        .unwrap()
        .unwrap();
    request.await.unwrap();
    assert_eq!(control.active_requests.load(Ordering::Acquire), 0);
    let weak = Arc::downgrade(&control);
    drop(control);
    timeout(Duration::from_secs(3), async {
        while weak.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a delayed stop must not retain Control after shutdown/owner close");
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn starting_signal_acknowledges_saved_intent_without_a_live_handle() {
    let (_root, owner, control, _requests, server) = fixture().await;
    let thread = rpc(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            model: None,
            effort: None,
            cwd: None,
            parent_id: None,
        }),
    )
    .await;
    let thread = thread["thread_id"].as_str().unwrap().to_owned();
    // This fixture is intentionally DB-only; the other cases exercise real OS
    // startup. It isolates the public acceptance/intent boundary without I/O.
    control.store.database.call(move|db| {
        db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('signal-run',?1,'live','now')",[thread])?;
        db.execute_batch("INSERT INTO turn(id,thread_id,run_id,state,created_at)SELECT 'signal-turn',thread_id,id,'running','now' FROM run WHERE id='signal-run';
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('stop-starting','signal-run','signal-turn','starting-stop','starting','/','{}','UTF-8','UTF-8','open','now');
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('eof-starting','signal-run','signal-turn','starting-eof','starting','/','{}','UTF-8','UTF-8','open','now');")?;
        Ok(())
    }).await.unwrap();
    let mut changes = control.subscribe();
    for (execution, eof) in [("stop-starting", false), ("eof-starting", true)] {
        let target = ExecutionTarget {
            execution_id: execution.into(),
        };
        let result = rpc(
            &control,
            if eof {
                Command::ShellEof(target)
            } else {
                Command::ShellStop(target)
            },
        )
        .await;
        assert_eq!(
            result["accepted"], true,
            "starting intent must not degrade into a state query"
        );
        assert!(
            matches!(changes.try_recv(), Ok(EngineEvent::Changed { .. })),
            "saved intent must notify reactive observers before handle registration"
        );
        wait_saved_signal(&control, execution, eof).await;
    }
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_grace_owner_does_not_swallow_another_saved_stop() {
    let (_root, owner, control, mut requests, server) = fixture().await;
    start(&control).await;
    let (_, reply) = incoming(&mut requests).await;
    reply.send(tool("cat")).unwrap();
    let (execution, handle) = timeout(Duration::from_secs(3), async {
        loop {
            let shell = control
                .live
                .test_shells()
                .lock()
                .await
                .iter()
                .next()
                .map(|(id, s)| (id.clone(), s.handle.clone()));
            if let Some(shell) = shell {
                break shell;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Isolate a pending self-stop owner which will fail. Another caller's
    // already-saved stop must still reach the real registered process.
    let (result, receive) = tokio::sync::watch::channel(None);
    control.runtime_operations.lock().await.insert(
        "failed-grace-owner".into(),
        RuntimeOperation {
            fingerprint: Value::Null,
            deferred_shell_stop: Some(execution.clone()),
            receive,
        },
    );
    let target = execution.clone();
    control
        .store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE shell_execution SET state='stopping' WHERE id=?1",
                [target],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let registering = control.clone();
    let applying =
        tokio::spawn(async move { registering.reapply_shell_intent(&execution, &handle).await });
    timeout(Duration::from_secs(3), async {
        while result.receiver_count() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    result
        .send(Some(Err(ApiError::new(
            ErrorCode::OperationConflict,
            "fixture owner rejected",
        ))))
        .unwrap();
    applying.await.unwrap().unwrap();
    control
        .runtime_operations
        .lock()
        .await
        .remove("failed-grace-owner");
    let (_, reply) = incoming(&mut requests).await;
    reply.send(answer()).unwrap();
    control.shutdown().await.unwrap();
    assert!(control.live.test_shells().lock().await.is_empty());
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_self_stop_owner_does_not_preempt_another_saved_reply_grace() {
    let (_root, owner, control, mut requests, server) = fixture().await;
    start(&control).await;
    let (_, reply) = incoming(&mut requests).await;
    reply.send(tool("cat")).unwrap();
    let (execution, handle) = timeout(Duration::from_secs(3), async {
        loop {
            let shell = control
                .live
                .test_shells()
                .lock()
                .await
                .iter()
                .next()
                .map(|(id, s)| (id.clone(), s.handle.clone()));
            if let Some(shell) = shell {
                break shell;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (failed, failed_watch) = tokio::sync::watch::channel(Some(Err(ApiError::new(
        ErrorCode::OperationConflict,
        "fixture rejected owner",
    ))));
    let (accepted, accepted_watch) =
        tokio::sync::watch::channel(Some(Ok(json!({"accepted":true}))));
    for (id, receive) in [
        ("failed-owner", failed_watch),
        ("accepted-owner", accepted_watch),
    ] {
        control.runtime_operations.lock().await.insert(
            id.into(),
            RuntimeOperation {
                fingerprint: Value::Null,
                deferred_shell_stop: Some(execution.clone()),
                receive,
            },
        );
    }
    let target = execution.clone();
    control
        .store
        .database
        .call(move |db| {
            db.execute(
                "UPDATE shell_execution SET state='stopping' WHERE id=?1",
                [target],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    control
        .reapply_shell_intent(&execution, &handle)
        .await
        .unwrap();
    assert_eq!(
        handle
            .write("STILL_OWNS_REPLY_GRACE", "UTF-8")
            .await
            .unwrap()
            .status,
        crate::shell::StdinWriteStatus::Complete
    );
    for id in ["failed-owner", "accepted-owner"] {
        control.runtime_operations.lock().await.remove(id);
    }
    drop((failed, accepted));
    control.signal_shell(execution, false, None).await.unwrap();
    let (_, reply) = incoming(&mut requests).await;
    reply.send(answer()).unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn saved_self_stop_replay_does_not_create_a_new_grace_owner_or_stop_effect() {
    let (_root, owner, control, _requests, server) = fixture().await;
    let thread = rpc(
        &control,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            connection: None,
            model: None,
            effort: None,
            cwd: None,
            parent_id: None,
        }),
    )
    .await;
    let thread = thread["thread_id"].as_str().unwrap().to_owned();
    let target = thread.clone();
    control.store.database.call(move|db|{
        db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('replay-run',?1,'live','now')",[target])?;
        db.execute_batch("INSERT INTO turn(id,thread_id,run_id,state,created_at)SELECT 'replay-turn',thread_id,id,'running','now' FROM run WHERE id='replay-run';
            INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at)VALUES('replay-shell','replay-run','replay-turn','replay-shell-start','starting','/','{}','UTF-8','UTF-8','open','now');")?;Ok(())
    }).await.unwrap();
    let origin = Origin {
        instance_id: control.store.instance_id.clone(),
        thread_id: thread,
        run_id: "replay-run".into(),
        turn_id: "replay-turn".into(),
        execution_id: "replay-shell".into(),
        token: "fixture-replay-token".into(),
    };
    control
        .origins
        .lock()
        .await
        .insert(origin.token.clone(), origin.clone());
    let operation = format!("{}:saved-self-stop", control.store.instance_id);
    let mut request = Request::new(
        operation.clone(),
        Command::ShellStop(ExecutionTarget {
            execution_id: origin.execution_id.clone(),
        }),
    );
    request.origin = Some(origin.clone());
    assert!(matches!(
        control
            .store
            .reserve_runtime(request.clone(), Some(origin))
            .await
            .unwrap(),
        db::RuntimeReservation::New
    ));
    control
        .store
        .finalize_runtime(operation.clone(), Ok(json!({"accepted":true})))
        .await
        .unwrap();
    let response = control.request(request).await;
    assert!(response.ok);
    timeout(Duration::from_secs(3), async {
        loop {
            if !control
                .runtime_operations
                .lock()
                .await
                .contains_key(&operation)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a saved result must not schedule another 60-second grace");
    control
        .store
        .database
        .call(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT state FROM shell_execution WHERE id='replay-shell'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "starting"
            );
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn largest_configured_reply_grace_preserves_library_bounds_and_remains_cancellable() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    // Largest TOML integer is a valid positive u64 configuration value. Reuse
    // Tokio sleep's existing safe-delay policy instead of overflowing Instant.
    std::fs::write(&config, format!("[io]\nself_reply_grace_ms={}\n", i64::MAX)).unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, root.path().into());
    assert_eq!(control.store.config.io.self_reply_grace_ms, i64::MAX as u64);
    let delay = control.reply_grace();
    tokio::pin!(delay);
    tokio::select! {biased;
        _ = &mut delay => panic!("largest configured grace must remain pending before freeze"),
        _ = tokio::task::yield_now() => {}
    }
    control.prepare_shutdown().await.unwrap();
    timeout(Duration::from_secs(3), delay).await.unwrap();
    control.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
