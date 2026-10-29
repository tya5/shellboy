#[cfg(unix)]
use super::endpoint::SocketFile;
use super::*;
use crate::protocol::{Command, MessageSendArgs, Response};
use crate::{app::Endpoint, protocol::PROTOCOL_VERSION};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::DuplexStream;

type Client = JoinHandle<Result<Response<serde_json::Value>, ApiError>>;
struct FakeAcceptor {
    incoming: mpsc::Receiver<io::Result<DuplexStream>>,
    calls: Arc<AtomicUsize>,
    #[cfg(unix)]
    _listener: tokio::net::UnixListener,
}
impl Acceptor for FakeAcceptor {
    type Stream = DuplexStream;
    async fn accept(&mut self) -> io::Result<DuplexStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.incoming
            .recv()
            .await
            .unwrap_or_else(|| Err(io::ErrorKind::BrokenPipe.into()))
    }
}
struct Fixture {
    root: tempfile::TempDir,
    server: LocalServer,
    input: mpsc::Sender<io::Result<DuplexStream>>,
    incoming: mpsc::Receiver<Invocation>,
    calls: Arc<AtomicUsize>,
    endpoint: Endpoint,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let (listener, socket) = {
            let folder = root.path().join("private");
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&folder)
                .unwrap();
            let path = folder.join("s");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            (listener, Some(SocketFile::capture(path).unwrap()))
        };
        #[cfg(unix)]
        let address = socket.as_ref().map_or_else(String::new, |socket| {
            socket.path.to_string_lossy().into_owned()
        });
        #[cfg(windows)]
        let address = String::new();
        let endpoint = Endpoint {
            protocol_version: PROTOCOL_VERSION,
            instance_id: "fixture".into(),
            transport: "fixture".into(),
            address,
        };
        let metadata = root.path().join("endpoint.json");
        std::fs::write(&metadata, serde_json::to_vec(&endpoint).unwrap()).unwrap();
        let (input, accept) = mpsc::channel(MAX_CONNECTIONS + 2);
        let (requests, incoming) = mpsc::channel(MAX_CONNECTIONS + 2);
        let calls = Arc::new(AtomicUsize::new(0));
        let server = LocalServer::start(
            FakeAcceptor {
                incoming: accept,
                calls: calls.clone(),
                #[cfg(unix)]
                _listener: listener,
            },
            "fixture".into(),
            requests,
            EndpointFiles {
                metadata,
                instance: "fixture".into(),
                #[cfg(unix)]
                socket,
            },
        );
        Self {
            root,
            server,
            input,
            incoming,
            calls,
            endpoint,
        }
    }
    async fn connect(&self, number: usize) -> Client {
        self.connect_command(
            number,
            Command::MessageSend(MessageSendArgs {
                thread_id: Some("t".into()),
                text: "fixture".into(),
            }),
        )
        .await
    }
    async fn connect_command(&self, number: usize, command: Command) -> Client {
        let (client, server) = tokio::io::duplex(4096);
        self.input.send(Ok(server)).await.unwrap();
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            super::super::call_stream(
                client,
                endpoint,
                command,
                Some(format!("fixture:{number}")),
                None,
            )
            .await
        })
    }
    async fn pending_connection(&self) -> DuplexStream {
        use super::super::{Hello, Welcome};
        use crate::protocol::{read_frame, write_frame};
        let (mut client, server) = tokio::io::duplex(4096);
        self.input.send(Ok(server)).await.unwrap();
        write_frame(
            &mut client,
            &Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        read_frame::<_, Welcome>(&mut client)
            .await
            .unwrap()
            .unwrap();
        client
    }
    async fn accepted(&mut self) -> Invocation {
        tokio::time::timeout(Duration::from_secs(2), self.incoming.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn full_work_admission_does_not_block_status_or_stop_and_remains_bounded() {
    use crate::protocol::{RunTarget, StatusArgs, WaitArgs, WaitCondition};
    let mut fixture = Fixture::new();
    let mut clients = Vec::new();
    let mut accepted = Vec::new();
    for number in 0..MAX_IPC_WORK {
        clients.push(
            fixture
                .connect_command(
                    number,
                    Command::Wait(WaitArgs {
                        conditions: vec![WaitCondition::RunEnded {
                            run_id: "run".into(),
                        }],
                        timeout_ms: 60000,
                    }),
                )
                .await,
        );
        accepted.push(fixture.accepted().await);
    }
    for (number, command) in [
        (MAX_IPC_WORK, Command::Status(StatusArgs::default())),
        (
            MAX_IPC_WORK + 1,
            Command::RunStop(RunTarget {
                run_id: "run".into(),
            }),
        ),
    ] {
        let client = fixture.connect_command(number, command).await;
        let invocation = tokio::time::timeout(Duration::from_secs(1), fixture.incoming.recv())
            .await
            .expect("ordinary pending work must leave stop and status admission available")
            .unwrap();
        invocation
            .reply
            .send(Response::success(
                invocation.request.operation_id,
                serde_json::json!({"observed":true}),
            ))
            .unwrap();
        assert!(client.await.unwrap().unwrap().ok);
    }
    fixture.server.shutdown().await.unwrap();
    for client in clients {
        let _ = client.await;
    }
    drop(accepted);
}

#[tokio::test]
async fn each_reserved_class_is_bounded_and_releases_only_its_own_capacity() {
    use crate::protocol::{ExecutionTarget, RunTarget, StatusArgs};
    let mut fixture = Fixture::new();
    let mut clients = Vec::new();
    let mut ordinary = Vec::new();
    let mut controls = Vec::new();
    let mut observations = Vec::new();
    for number in 0..MAX_IPC_WORK {
        clients.push(fixture.connect(number).await);
        ordinary.push(fixture.accepted().await);
    }
    for number in 0..MAX_CONTROL_WORK {
        clients.push(
            fixture
                .connect_command(
                    100 + number,
                    Command::RunStop(RunTarget {
                        run_id: "run".into(),
                    }),
                )
                .await,
        );
        controls.push(fixture.accepted().await);
    }
    for number in 0..MAX_OBSERVATION_WORK {
        clients.push(
            fixture
                .connect_command(200 + number, Command::Status(StatusArgs::default()))
                .await,
        );
        observations.push(fixture.accepted().await);
    }
    for (number, command) in [
        (
            300,
            Command::RunStop(RunTarget {
                run_id: "run".into(),
            }),
        ),
        (
            301,
            Command::ShellStop(ExecutionTarget {
                execution_id: "execution".into(),
            }),
        ),
        (302, Command::Status(StatusArgs::default())),
    ] {
        let response = fixture
            .connect_command(number, command)
            .await
            .await
            .unwrap()
            .unwrap();
        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, ErrorCode::InvalidState);
        assert!(fixture.incoming.try_recv().is_err());
    }
    drop(controls.pop().unwrap());
    drop(observations.pop().unwrap());
    for (number, command) in [
        (
            400,
            Command::ShellStop(ExecutionTarget {
                execution_id: "execution".into(),
            }),
        ),
        (401, Command::Status(StatusArgs::default())),
    ] {
        let client = fixture.connect_command(number, command).await;
        let invocation = fixture.accepted().await;
        invocation
            .reply
            .send(Response::success(
                invocation.request.operation_id,
                serde_json::json!({"saved":true}),
            ))
            .unwrap();
        assert!(client.await.unwrap().unwrap().ok);
    }
    assert_eq!(ordinary.len(), MAX_IPC_WORK);
    fixture.server.shutdown().await.unwrap();
    for client in clients {
        let _ = client.await;
    }
}

#[tokio::test]
async fn resource_accept_error_recovers_without_losing_the_listener() {
    let mut fixture = Fixture::new();
    let failure = fixture.server.failures();
    #[cfg(unix)]
    let resource_error = io::Error::from_raw_os_error(libc::EMFILE);
    #[cfg(windows)]
    let resource_error = io::Error::from_raw_os_error(
        windows_sys::Win32::Foundation::ERROR_TOO_MANY_OPEN_FILES as i32,
    );
    fixture.input.send(Err(resource_error)).await.unwrap();
    let client = fixture.connect(1).await;
    let invocation = fixture.accepted().await;
    invocation
        .reply
        .send(Response::success(
            invocation.request.operation_id,
            serde_json::json!({"saved": true}),
        ))
        .unwrap();
    assert!(client.await.unwrap().unwrap().ok);
    assert!(failure.borrow().is_none());
    fixture.server.shutdown().await.unwrap();
    assert!(!fixture.root.path().join("endpoint.json").exists());
}

#[tokio::test]
async fn full_connection_set_defers_accept_until_a_connection_releases() {
    let mut fixture = Fixture::new();
    let mut clients = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        clients.push(fixture.pending_connection().await);
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), MAX_CONNECTIONS);
    let extra = fixture.connect(MAX_CONNECTIONS).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), fixture.incoming.recv())
            .await
            .is_err()
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), MAX_CONNECTIONS);
    drop(clients.pop().unwrap());
    let next = fixture.accepted().await;
    assert_eq!(
        next.request.operation_id,
        format!("fixture:{MAX_CONNECTIONS}")
    );
    next.reply
        .send(Response::success(
            next.request.operation_id,
            serde_json::json!({"saved":true}),
        ))
        .unwrap();
    assert!(extra.await.unwrap().unwrap().ok);
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn fatal_accept_failure_cleans_endpoint_notifies_once_and_preserves_accepted_work() {
    let mut fixture = Fixture::new();
    let mut failure = fixture.server.failures();
    let client = fixture.connect(1).await;
    let invocation = fixture.accepted().await;
    fixture
        .input
        .send(Err(io::ErrorKind::PermissionDenied.into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), failure.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        failure.borrow_and_update().as_ref().unwrap().code,
        ErrorCode::IoFailed
    );
    assert!(!fixture.root.path().join("endpoint.json").exists());
    #[cfg(unix)]
    {
        assert!(!std::path::Path::new(&fixture.endpoint.address).exists());
        assert!(!fixture.root.path().join("private").exists());
    }
    assert!(invocation.disconnected.is_cancelled());
    let error = client.await.unwrap().unwrap_err();
    assert_eq!(
        error.details.unwrap()["request_may_have_been_applied"],
        true
    );
    // The accepted operation is owned by the request consumer, not the IPC task.
    let db = crate::storage::DbWorker::open(fixture.root.path().join("fixture.db"))
        .await
        .unwrap();
    let operation = invocation.request.operation_id.clone();
    db.call(move |connection| {
        connection.execute(
            "INSERT INTO app_meta(key,value)VALUES(?1,'committed')",
            [&operation],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    db.shutdown().await.unwrap();
    assert!(
        invocation
            .reply
            .send(Response::success(
                invocation.request.operation_id,
                serde_json::json!({"saved":true})
            ))
            .is_err()
    );
    assert!(fixture.server.shutdown().await.is_err());
    assert!(
        failure.changed().await.is_err(),
        "there is only one terminal notification"
    );
}

#[tokio::test]
async fn cleanup_preserves_an_endpoint_replaced_by_another_instance() {
    let fixture = Fixture::new();
    let mut replacement = fixture.endpoint.clone();
    replacement.instance_id = "different-instance".into();
    std::fs::write(
        fixture.root.path().join("endpoint.json"),
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();
    fixture.server.shutdown().await.unwrap();
    let actual: Endpoint =
        serde_json::from_slice(&std::fs::read(fixture.root.path().join("endpoint.json")).unwrap())
            .unwrap();
    assert_eq!(actual.instance_id, replacement.instance_id);
}

#[tokio::test]
async fn shutdown_cancels_a_backoff_without_waiting_for_its_deadline() {
    let stop = CancellationToken::new();
    let wait = wait_backoff(&stop, Duration::from_secs(3600));
    tokio::pin!(wait);
    assert!(futures_util::poll!(&mut wait).is_pending());
    stop.cancel();
    assert!(
        !tokio::time::timeout(Duration::from_secs(2), wait)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn request_frame_timeout_releases_a_full_connection_slot_without_dispatch() {
    use crate::protocol::MAX_FRAME_BYTES;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut fixture = Fixture::new();
    let mut clients = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        clients.push(fixture.pending_connection().await);
    }
    clients[0]
        .write_all(&(MAX_FRAME_BYTES as u32).to_be_bytes())
        .await
        .unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), MAX_CONNECTIONS);
    // Observe expiry before starting the next client's own five-second hello
    // timer. Under parallel test load both timers could become ready together,
    // making the client abandon a connection before its request was dispatched.
    // Keep the same overall six-second bound for expiry AND resumed dispatch.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    let mut byte = [0u8; 1];
    assert_eq!(
        tokio::time::timeout_at(deadline, clients[0].read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0,
        "the partial frame connection must close without a response"
    );
    let next = fixture.connect(MAX_CONNECTIONS).await;
    let invocation = tokio::time::timeout_at(deadline, fixture.incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        invocation.request.operation_id,
        format!("fixture:{MAX_CONNECTIONS}")
    );
    invocation
        .reply
        .send(Response::success(
            invocation.request.operation_id,
            serde_json::json!({"saved":true}),
        ))
        .unwrap();
    assert!(next.await.unwrap().unwrap().ok);
    assert!(
        fixture.incoming.try_recv().is_err(),
        "partial frames must never dispatch"
    );
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn disconnected_accepted_work_keeps_all_slots_until_consumer_completion() {
    let mut fixture = Fixture::new();
    let mut clients = Vec::new();
    let mut accepted = Vec::new();
    for number in 0..MAX_IPC_WORK {
        clients.push(fixture.connect(number).await);
        accepted.push(fixture.accepted().await);
    }
    for client in clients {
        client.abort();
        let _ = client.await;
    }
    for invocation in &accepted {
        tokio::time::timeout(Duration::from_secs(2), invocation.disconnected.cancelled())
            .await
            .unwrap();
    }
    let busy = fixture.connect(MAX_IPC_WORK).await.await.unwrap().unwrap();
    assert!(!busy.ok);
    let error = busy.error.unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(
        error.details.unwrap()["request_may_have_been_applied"],
        false
    );
    assert!(
        fixture.incoming.try_recv().is_err(),
        "capacity refusal must not dispatch"
    );
    drop(accepted.pop().unwrap());
    let next = fixture.connect(MAX_IPC_WORK).await;
    let invocation = fixture.accepted().await;
    assert_eq!(
        invocation.request.operation_id,
        format!("fixture:{MAX_IPC_WORK}")
    );
    invocation
        .reply
        .send(Response::success(
            invocation.request.operation_id,
            serde_json::json!({"saved":true}),
        ))
        .unwrap();
    assert!(next.await.unwrap().unwrap().ok);
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permanent_listener_failure_reaches_the_frontend_once_while_local_work_stays_live() {
    use crate::{
        app::DataOwner,
        control::Control,
        frontend,
        ui::{UiAction, UiUpdate},
    };
    let Fixture {
        root,
        server,
        input,
        ..
    } = Fixture::new();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config),
        Some(root.path().join("data")),
        root.path().into(),
    )
    .await
    .unwrap();
    let database = owner.db.clone();
    let control = Control::new(&owner, root.path().into());
    let (actions, receive) = mpsc::channel(8);
    let (updates, received) = std::sync::mpsc::sync_channel(128);
    // The adapter creates its existing worker lazily; this fixture sends no
    // clipboard action and never reads or changes the host clipboard.
    let bridge = tokio::spawn(frontend::run(
        control,
        receive,
        updates,
        root.path().into(),
        owner,
        server,
    ));
    input
        .send(Err(io::ErrorKind::PermissionDenied.into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match received.try_recv() {
                Ok(UiUpdate::Status { text, .. }) if text.contains("CLI受付が停止しました") =>
                {
                    break;
                }
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(error) => panic!("frontend stopped before notice: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(
        !bridge.is_finished(),
        "a transport failure does not stop the TUI backend"
    );
    database
        .call(|connection| {
            connection.execute("INSERT INTO app_meta VALUES('ipc-ui-live','committed')", [])?;
            Ok(())
        })
        .await
        .unwrap();
    actions
        .send(UiAction::RenderWidth { width: 80 })
        .await
        .unwrap();
    actions.send(UiAction::Quit).await.unwrap();
    // The 2-second bound above covers the listener-failure notification. The
    // notification is published only after LocalServer's endpoint cleanup, so
    // the listener task is already done here. Joining the frontend additionally
    // drains Control, closes SQLite and shuts down diagnostics (whose default
    // grace alone is 5 seconds). This is an eventual owner-cleanup assertion,
    // not another IPC response deadline. Match the existing frontend fixture's
    // cleanup watchdog while consuming updates like the real UI.
    let cleanup_seconds = if cfg!(windows) { 30 } else { 15 };
    let mut bridge = bridge;
    let mut duplicates = 0;
    let mut last_progress = None;
    let joined = tokio::time::timeout(Duration::from_secs(cleanup_seconds), async {
        let mut tick = tokio::time::interval(Duration::from_millis(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            while let Ok(update) = received.try_recv() {
                match update {
                    UiUpdate::Status { text, .. } if text.contains("CLI受付が停止しました") =>
                    {
                        duplicates += 1;
                    }
                    UiUpdate::ShutdownProgress {
                        elapsed_ms,
                        detail,
                        force_available,
                    } => {
                        last_progress = Some((elapsed_ms, detail, force_available));
                    }
                    _ => {}
                }
            }
            tokio::select! {
                result = &mut bridge => return result,
                _ = tick.tick() => {},
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("frontend owner-cleanup watchdog expired; last progress: {last_progress:?}")
    });
    let error = joined.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(
        matches!(
            database.call(|_| Ok(())).await.unwrap_err(),
            crate::storage::StorageError::Closed
        ),
        "the owner must actually close its database before the bridge returns"
    );
    duplicates += received.try_iter().filter(|update| matches!(update, UiUpdate::Status { text, .. } if text.contains("CLI受付が停止しました"))).count();
    assert_eq!(
        duplicates, 0,
        "the closed watch must not produce repeated UI notices"
    );
    // The normal, low-volume fixture drains the existing diagnostic writer.
    // Stage times are captured at callers, not delayed writer wall-clock times.
    let log = std::fs::read_to_string(root.path().join("data/logs/shellboy.log")).unwrap();
    let records = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        records.iter().all(|record| record["dropped"] == 0),
        "normal shutdown dropped diagnostics: {records:?}"
    );
    let expected_frontend = [
        "listener",
        "draft_enqueue",
        "accepted_sends",
        "draft_save",
        "interrupt_capture",
        "control_tasks",
        "read_workers",
        "images",
        "control",
        "workers",
    ]
    .into_iter()
    .flat_map(|stage| [(stage, "begin"), (stage, "end")])
    .chain(std::iter::once(("owner", "begin")))
    .collect::<Vec<_>>();
    let expected_owner = vec![
        ("database", "begin"),
        ("database", "end"),
        ("endpoint", "begin"),
        ("endpoint", "end"),
        ("diagnostics", "begin"),
    ];
    for (scope, expected) in [("frontend", expected_frontend), ("owner", expected_owner)] {
        let traces = records
            .iter()
            .filter(|record| record["event"] == "shutdown" && record["shutdown"]["scope"] == scope)
            .map(|record| &record["shutdown"])
            .collect::<Vec<_>>();
        let stages = traces
            .iter()
            .map(|trace| {
                (
                    trace["stage"].as_str().unwrap(),
                    trace["boundary"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            stages, expected,
            "missing fixed shutdown stages for {scope}"
        );
        let elapsed = traces
            .iter()
            .map(|trace| trace["elapsed_ms"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert!(
            elapsed.windows(2).all(|pair| pair[0] <= pair[1]),
            "non-monotonic caller elapsed time in {scope}: {elapsed:?}"
        );
        assert!(
            traces
                .iter()
                .all(|trace| trace.as_object().unwrap().len() == 4),
            "shutdown traces must contain only fixed scope/stage/boundary and elapsed time"
        );
    }
}

#[test]
fn startup_failures_explain_the_phase_without_mutation_recovery_advice() {
    for reason in [
        "existing_socket_live",
        "existing_socket_probe_timeout",
        "existing_socket_probe_failed",
        "namespace_lease_failed",
        "bind_failed",
    ] {
        let error = super::startup_error(
            reason,
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert_eq!(error.code, crate::protocol::ErrorCode::IoFailed);
        assert!(!error.message.contains("Permission denied"));
        let hint = error.hint.as_deref().unwrap();
        assert!(!hint.contains("receipt"));
        assert!(!hint.contains("Restart the owner"));
        let details = error.details.as_ref().unwrap();
        assert_eq!(details["phase"], "startup");
        assert_eq!(details["reason"], reason);
        assert!(details["cause"].is_string());
        assert_eq!(super::startup_context(error.clone()), error);
    }
    let nested = super::startup_context(super::server_error("socket metadata failed", "not found"));
    assert_eq!(
        nested.details.as_ref().unwrap()["reason"],
        "endpoint_preparation_failed"
    );
    assert!(!nested.hint.as_deref().unwrap().contains("receipt"));
    // Runtime listener failure is distinct: it can follow accepted mutations.
    assert!(
        super::server_error("listener stopped", "fixture")
            .hint
            .unwrap()
            .contains("receipts")
    );
}
