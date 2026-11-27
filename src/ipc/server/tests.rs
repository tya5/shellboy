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
pub(crate) struct Fixture {
    pub(crate) root: tempfile::TempDir,
    pub(crate) server: LocalServer,
    pub(crate) input: mpsc::Sender<io::Result<DuplexStream>>,
    pub(crate) incoming: mpsc::Receiver<Invocation>,
    calls: Arc<AtomicUsize>,
    endpoint: Endpoint,
}
impl Fixture {
    fn new() -> Self {
        Self::for_instance("fixture")
    }
    pub(crate) fn for_instance(instance: &str) -> Self {
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
            owner_version: None,
            protocol_version: PROTOCOL_VERSION,
            instance_id: instance.into(),
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
            instance.into(),
            requests,
            EndpointFiles {
                metadata,
                instance: instance.into(),
                #[cfg(unix)]
                socket,
            },
            Arc::default(),
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
                sender_label: None,
                deliver: crate::protocol::Deliver::Next,
                thread_id: Some("t".into()),
                text: "fixture".into(),
            }),
        )
        .await
    }
    pub(crate) async fn connect_command(&self, number: usize, command: Command) -> Client {
        let (client, server) = tokio::io::duplex(4096);
        self.input.send(Ok(server)).await.unwrap();
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            let operation = format!("{}:{number}", endpoint.instance_id);
            super::super::call_stream(client, endpoint, command, Some(operation), None).await
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
        crate::test_watchdog::watchdog("src/ipc/server/tests.rs: accepted", self.incoming.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test(start_paused = true)]
async fn cli_wait_retries_only_definite_admission_refusals_until_deadline() {
    use crate::protocol::{WaitArgs, WaitCondition};
    use serde_json::json;
    let args = WaitArgs {
        conditions: vec![WaitCondition::RunEnded {
            run_id: "run".into(),
        }],
        timeout_ms: 2350,
        after: Some("fixture-cursor".into()),
    };
    let mut attempts = Vec::new();
    let response = super::super::retry_cli_wait(args.clone(), "fixture:wait", |args| {
        attempts.push(args.timeout_ms);
        std::future::ready(Ok(Response::failure(
            "fixture:wait",
            ApiError::new(ErrorCode::InvalidState, "busy")
                .details(json!({"phase": "admission", "request_may_have_been_applied": false})),
        )))
    })
    .await
    .unwrap();
    assert_eq!(attempts, [2350, 2300, 2200, 2000, 1600, 800]);
    assert_eq!(response.operation_id, "fixture:wait");
    let result = response.result.unwrap();
    assert_eq!(result["status"], "timeout");
    assert_eq!(result["cursor"], "fixture-cursor");
    assert_eq!(result["snapshot"], json!({"conditions": []}));
    assert_eq!(result["admission_wait_ms"], 2350);
    assert_eq!(result["admission_refusals"], 6);

    for code in [ErrorCode::InvalidState, ErrorCode::IoFailed] {
        let mut attempts = 0;
        let response = super::super::retry_cli_wait(args.clone(), "fixture:wait", |_| {
            attempts += 1;
            std::future::ready(Ok(Response::failure(
                "fixture:wait",
                ApiError::new(code, "not a definite admission refusal")
                    .details(json!({"phase": "admission", "request_may_have_been_applied": true})),
            )))
        })
        .await
        .unwrap();
        assert_eq!(attempts, 1);
        assert_eq!(response.error.unwrap().code, code);
    }
}

#[tokio::test]
async fn parked_waits_leave_ordinary_and_stop_admission_available() {
    use crate::protocol::{
        EventsWaitArgs, Request, WaitArgs, WaitCondition, read_frame, write_frame,
    };
    let mut fixture = Fixture::new();
    let mut clients = Vec::new();
    let mut accepted = Vec::new();
    let wait = || {
        Command::Wait(WaitArgs {
            after: None,
            conditions: vec![WaitCondition::RunEnded {
                run_id: "run".into(),
            }],
            timeout_ms: 60000,
        })
    };
    for number in 0..32 {
        let command = if number % 2 == 0 {
            wait()
        } else {
            Command::EventsWait(EventsWaitArgs {
                after: None,
                thread_ids: Vec::new(),
                timeout_ms: 60000,
                include_text: false,
                include_reasoning_activity: false,
                include_output: false,
            })
        };
        clients.push(fixture.connect_command(number, command).await);
        accepted.push(fixture.accepted().await);
    }

    let mut rejected = fixture.pending_connection().await;
    write_frame(&mut rejected, &Request::new("fixture:32", wait()))
        .await
        .unwrap();
    let response = crate::test_watchdog::watchdog(
        "wait capacity refusal",
        read_frame::<_, Response<serde_json::Value>>(&mut rejected),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let error = response.error.unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);
    let details = error.details.unwrap();
    assert_eq!(details["phase"], "admission");
    assert_eq!(details["request_may_have_been_applied"], false);
    assert!(details.get("next_operation").is_none());
    let eof = crate::test_watchdog::watchdog(
        "refused wait closes",
        read_frame::<_, Response<serde_json::Value>>(&mut rejected),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(eof.is_none());
    assert!(fixture.incoming.try_recv().is_err());

    let ordinary = fixture.connect(33).await;
    let stop = fixture
        .connect_command(
            34,
            Command::RunStop(crate::protocol::RunStopTarget::Run {
                run_id: "run".into(),
            }),
        )
        .await;
    for _ in 0..2 {
        let invocation = fixture.accepted().await;
        invocation
            .reply
            .send(Response::success(
                invocation.request.operation_id,
                serde_json::json!({"observed": true}),
            ))
            .unwrap();
    }
    assert!(ordinary.await.unwrap().unwrap().ok);
    assert!(stop.await.unwrap().unwrap().ok);
    assert!(clients.iter().all(|client| !client.is_finished()));
    drop(accepted);
    for client in clients {
        assert!(!client.await.unwrap().unwrap().ok);
    }
    fixture.server.shutdown().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn refused_wait_frame_survives_native_pipe_close() {
    use crate::protocol::EventsWaitArgs;
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    let address = format!(r"\\.\pipe\shellboy-wait-test-{}", uuid::Uuid::new_v4());
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&address)
        .unwrap();
    let client = ClientOptions::new().open(&address).unwrap();
    server.connect().await.unwrap();
    let command = Command::EventsWait(EventsWaitArgs {
        after: None,
        thread_ids: Vec::new(),
        timeout_ms: 60000,
        include_text: false,
        include_reasoning_activity: false,
        include_output: false,
    });
    let work = WorkAdmission::new();
    let refusals = work.refused.clone();
    let _held: Vec<_> = (0..MAX_WAIT_WORK)
        .map(|_| work.try_acquire(&command).unwrap())
        .collect();
    let (requests, mut incoming) = mpsc::channel(1);
    let served = tokio::spawn(serve_connection(
        server,
        "fixture".into(),
        requests,
        CancellationToken::new(),
        work,
    ));
    let endpoint = Endpoint {
        owner_version: None,
        protocol_version: PROTOCOL_VERSION,
        instance_id: "fixture".into(),
        transport: "pipe".into(),
        address,
    };
    let response = crate::test_watchdog::watchdog(
        "native pipe wait refusal",
        super::super::call_stream(client, endpoint, command, Some("fixture:pipe".into()), None),
    )
    .await
    .unwrap()
    .unwrap();
    let error = response.error.unwrap();
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(refusals.snapshot().wait.refused, 1);
    assert_eq!(refusals.snapshot().ordinary.refused, 0);
    let details = error.details.unwrap();
    assert_eq!(details["phase"], "admission");
    assert_eq!(details["request_may_have_been_applied"], false);
    assert!(details.get("next_operation").is_none());
    served.await.unwrap().unwrap();
    assert!(incoming.try_recv().is_err());
}

#[tokio::test]
async fn full_work_admission_does_not_block_status_or_stop_and_remains_bounded() {
    use crate::protocol::StatusArgs;
    let mut fixture = Fixture::new();
    let mut clients = Vec::new();
    let mut accepted = Vec::new();
    for number in 0..MAX_IPC_WORK {
        clients.push(fixture.connect(number).await);
        accepted.push(fixture.accepted().await);
    }
    for (number, command) in [
        (MAX_IPC_WORK, Command::Status(StatusArgs::default())),
        (
            MAX_IPC_WORK + 1,
            Command::RunStop(crate::protocol::RunStopTarget::Run {
                run_id: "run".into(),
            }),
        ),
    ] {
        let client = fixture.connect_command(number, command).await;
        let invocation = crate::test_watchdog::watchdog("src/ipc/server/tests.rs: full_work_admission_does_not_block_status_or_stop_and_remains_bounded", fixture.incoming.recv())
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
    drop(accepted);
    for client in clients {
        assert!(!client.await.unwrap().unwrap().ok);
    }
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn each_reserved_class_is_bounded_and_releases_only_its_own_capacity() {
    use crate::protocol::{ExecutionTarget, StatusArgs};
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
                    Command::RunStop(crate::protocol::RunStopTarget::Run {
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
            Command::RunStop(crate::protocol::RunStopTarget::Run {
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
    drop((ordinary, controls, observations));
    for client in clients {
        assert!(!client.await.unwrap().unwrap().ok);
    }
    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn memory_status_is_served_when_both_plain_status_permits_are_held() {
    use crate::protocol::StatusArgs;

    crate::test_watchdog::watchdog("isolated memory status admission", async {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let mut fixture = Fixture::for_instance(&owner.instance_id);
        let plain = || Command::Status(StatusArgs::default());
        let memory = || {
            Command::Status(StatusArgs {
                memory: true,
                ..Default::default()
            })
        };

        let mut clients = Vec::new();
        let mut held = Vec::new();
        // Hold dispatched requests at the owner boundary, not by timing a DB read.
        for number in 0..2 {
            clients.push(fixture.connect_command(number, plain()).await);
            held.push(fixture.accepted().await);
        }
        let rejected = fixture
            .connect_command(2, plain())
            .await
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rejected.error.unwrap().code, ErrorCode::InvalidState);

        let client = fixture.connect_command(3, memory()).await;
        let invocation = fixture.accepted().await;
        let rejected = fixture
            .connect_command(4, memory())
            .await
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rejected.error.unwrap().code, ErrorCode::InvalidState);
        assert!(fixture.incoming.try_recv().is_err());

        let (send, receive) = mpsc::channel(1);
        let dispatch = tokio::spawn(control.clone().serve(receive));
        assert!(send.send(invocation).await.is_ok());
        let response = client.await.unwrap().unwrap();
        assert!(response.ok, "{response:?}");
        assert_eq!(
            response.result.unwrap()["process"]["pid"],
            std::process::id()
        );
        drop(send);
        dispatch.await.unwrap();
        drop(held);
        for client in clients {
            assert!(!client.await.unwrap().unwrap().ok);
        }
        fixture.server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    })
    .await
    .unwrap();
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
    assert!(fixture.incoming.try_recv().is_err());
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
    crate::test_watchdog::watchdog("src/ipc/server/tests.rs: fatal_accept_failure_cleans_endpoint_notifies_once_and_preserves_accepted_work", failure.changed())
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
    fixture.server.stop_accepting().await;
    assert!(!invocation.disconnected.is_cancelled());
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
    invocation
        .reply
        .send(Response::success(
            invocation.request.operation_id,
            serde_json::json!({"saved":true}),
        ))
        .unwrap();
    assert_eq!(
        client.await.unwrap().unwrap().result.unwrap(),
        serde_json::json!({"saved":true})
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
        !crate::test_watchdog::watchdog(
            "src/ipc/server/tests.rs: shutdown_cancels_a_backoff_without_waiting_for_its_deadline",
            wait
        )
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
    // The shared watchdog protects observations; the product frame timer is unchanged.

    let mut byte = [0u8; 1];
    assert_eq!(
        crate::test_watchdog::watchdog("partial frame EOF", clients[0].read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0,
        "the partial frame connection must close without a response"
    );
    let next = fixture.connect(MAX_CONNECTIONS).await;
    let invocation =
        crate::test_watchdog::watchdog("dispatch after expired frame", fixture.incoming.recv())
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
        crate::test_watchdog::watchdog("src/ipc/server/tests.rs: disconnected_accepted_work_keeps_all_slots_until_consumer_completion", invocation.disconnected.cancelled())
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
    crate::test_watchdog::watchdog("src/ipc/server/tests.rs: permanent_listener_failure_reaches_the_frontend_once_while_local_work_stays_live", async {
        loop {
            match received.try_recv() {
                Ok(UiUpdate::Status { text, .. }) if text.contains("CLI intake stopped") =>
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
    let _cleanup_seconds = if cfg!(windows) { 30 } else { 15 };
    let mut bridge = bridge;
    let mut duplicates = 0;
    let mut last_progress = None;
    let joined = crate::test_watchdog::watchdog("src/ipc/server/tests.rs: permanent_listener_failure_reaches_the_frontend_once_while_local_work_stays_live", async {
        let mut tick = tokio::time::interval(Duration::from_millis(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            while let Ok(update) = received.try_recv() {
                match update {
                    UiUpdate::Status { text, .. } if text.contains("CLI intake stopped") =>
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
    duplicates += received.try_iter().filter(|update| matches!(update, UiUpdate::Status { text, .. } if text.contains("CLI intake stopped"))).count();
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
        "draft_enqueue",
        "accepted_sends",
        "draft_save",
        "control_tasks",
        "read_workers",
        "images",
        "workers",
    ]
    .into_iter()
    .flat_map(|stage| [(stage, "begin"), (stage, "end")])
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
        assert!(error.message.is_ascii());
        assert!(hint.is_ascii());
        assert!(error.message.ends_with('.'));
        assert!(hint.ends_with('.'));
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

#[test]
fn shutdown_uses_existing_stop_reserve_without_expanding_any_budget() {
    let work = WorkAdmission::new();
    let held: Vec<_> = (0..MAX_IPC_WORK)
        .map(|_| work.ordinary.clone().try_acquire_owned().unwrap())
        .collect();
    let command = Command::OwnerShutdown(crate::protocol::OwnerShutdownArgs {
        instance_id: "owner".into(),
    });
    assert!(!command.is_read_only());
    assert!(command.requires_receipt());
    assert!(!command.changes_context());
    assert!(!command.runtime_cancellable());
    let first = work.try_acquire(&command).unwrap();
    let second = work.try_acquire(&command).unwrap();
    assert_eq!(MAX_CONTROL_WORK, 2);
    assert!(work.try_acquire(&command).is_err());
    assert_eq!(work.ordinary.available_permits(), 0);
    drop((first, second, held));
    assert_eq!(work.control.available_permits(), MAX_CONTROL_WORK);
    assert_eq!(work.ordinary.available_permits(), MAX_IPC_WORK);
}
