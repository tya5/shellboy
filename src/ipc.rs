//! One-owner local IPC. Observer cancellation never cancels accepted mutations.
use crate::{
    app::Endpoint,
    protocol::{
        ApiError, Command, ErrorCode, Origin, PROTOCOL_VERSION, Request, Response, encode_frame,
        read_frame, write_encoded_frame, write_frame,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
mod peer;
mod server;
pub use server::LocalServer;

const FRAME_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct Hello {
    pub protocol_version: u32,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct Welcome {
    pub protocol_version: u32,
    pub instance_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
pub struct Invocation {
    pub(crate) reply_completed: Option<CancellationToken>,
    pub request: Request,
    pub reply: oneshot::Sender<Response<Value>>,
    /// Only wait/observation should consult this; accepted writes must continue.
    pub disconnected: CancellationToken,
    /// Retained by the consumer even after the transport observer disconnects.
    pub(crate) admission: OwnedSemaphorePermit,
}
fn io_error(_: std::io::Error) -> ApiError {
    ApiError::new(
        ErrorCode::AppNotRunning,
        "Cannot connect to the owner of this data directory",
    )
    .hint("Check --data-dir and start its owner if needed; inspect any known operation receipt before repeating a mutation")
}
fn endpoint_mismatch() -> ApiError {
    ApiError::new(
        ErrorCode::ProtocolMismatch,
        "Local endpoint metadata or owner instance changed",
    )
    .hint("Check the selected data directory and owner, refresh endpoint metadata, and reconnect")
}
fn protocol_mismatch(client: u32, owner: u32) -> ApiError {
    ApiError::new(
        ErrorCode::ProtocolMismatch,
        format!("CLI uses protocol {client} but the owner uses {owner}; run the CLI and owner from the same release"),
    )
    .hint("Run the CLI and owner from the same release; retrying unchanged binaries cannot resolve this version mismatch")
    .details(serde_json::json!({
        "client_protocol_version": client,
        "owner_protocol_version": owner,
        "request_may_have_been_applied": false
    }))
}

enum ConnectionOutcome {
    Reply(Response<Value>),
    PeerGone,
}

fn ipc_failure(mut error: ApiError, stage: &str, reason: &str, unapplied: bool) -> ApiError {
    let details = error.details.get_or_insert_with(|| serde_json::json!({}));
    if !details.is_object() {
        *details = serde_json::json!({"cause_details": details.take()});
    }
    let details = details.as_object_mut().expect("IPC failure details object");
    details.insert("stage".into(), Value::String(stage.into()));
    details
        .entry("reason")
        .or_insert_with(|| Value::String(reason.into()));
    if unapplied {
        details.insert("request_may_have_been_applied".into(), Value::Bool(false));
    }
    error
}

fn peer_read_outcome(
    read: std::io::Result<usize>,
    operation_id: &str,
    stage: &str,
    unapplied: bool,
) -> ConnectionOutcome {
    let (error, reason) = match read {
        Ok(0) => return ConnectionOutcome::PeerGone,
        Ok(_) => (
            ApiError::new(
                ErrorCode::InvalidArgument,
                "Unexpected bytes after IPC request",
            ),
            "unexpected_peer_byte",
        ),
        Err(error) => (
            ApiError::new(
                ErrorCode::IoFailed,
                format!("IPC peer read failed: {}", error.kind()),
            ),
            "peer_read_error",
        ),
    };
    ConnectionOutcome::Reply(Response::failure(
        operation_id,
        ipc_failure(error, stage, reason, unapplied),
    ))
}

async fn serve_connection<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    instance: String,
    requests: mpsc::Sender<Invocation>,
    stop: CancellationToken,
    work: server::WorkAdmission,
) -> Result<(), ApiError> {
    let read_request = async {
        let hello: Hello = tokio::time::timeout(FRAME_READ_TIMEOUT, read_frame(&mut stream))
            .await
            .map_err(|_| endpoint_mismatch())??
            .ok_or_else(endpoint_mismatch)?;
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(protocol_mismatch(hello.protocol_version, PROTOCOL_VERSION));
        }
        write_frame(
            &mut stream,
            &Welcome {
                protocol_version: PROTOCOL_VERSION,
                capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                instance_id: instance,
            },
        )
        .await?;
        let frame: Value = tokio::time::timeout(FRAME_READ_TIMEOUT, read_frame(&mut stream))
            .await
            .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Request frame read timed out"))??
            .ok_or_else(endpoint_mismatch)?;
        Ok::<_, ApiError>(frame)
    };
    let frame = tokio::select! {
        biased;
        result = read_request => result?,
        _ = stop.cancelled() => return Ok(()),
    };
    let request = Request::decode_frame(frame);
    let operation_id = match &request {
        Ok(request) if !request.operation_id.trim().is_empty() => request.operation_id.clone(),
        Err((Some(id), _)) => id.clone(),
        _ => return Err(endpoint_mismatch()),
    };
    let disconnected = CancellationToken::new();
    let _disconnected_guard = disconnected.clone().drop_guard();
    let reply_completed =
        matches!(&request, Ok(request) if matches!(request.command, Command::OwnerShutdown(_)))
            .then(CancellationToken::new);
    let _reply_completion_guard = reply_completed.clone().map(CancellationToken::drop_guard);
    let failure = |error, stage, reason, unapplied| {
        ConnectionOutcome::Reply(Response::failure(
            operation_id.clone(),
            ipc_failure(error, stage, reason, unapplied),
        ))
    };
    let stopping = |stage, unapplied| {
        failure(
            ApiError::new(ErrorCode::AppNotRunning, "Owner IPC transport is stopping"),
            stage,
            "transport_stopping",
            unapplied,
        )
    };
    let recovery = request
        .as_ref()
        .ok()
        .map(|request| (request.command.name(), request.command.requires_receipt()));
    let mut dispatched = false;
    let mut byte = [0];
    let outcome = 'request: {
        let request = match request {
            Ok(request) => request,
            Err((_, error)) => break 'request failure(error, "decode", "invalid_request", true),
        };
        if let Err(error) = request.validate() {
            break 'request failure(error, "validate", "invalid_request", true);
        }
        let admission = match work.try_acquire(&request.command) {
            Ok(permit) => permit,
            Err(error) => break 'request failure(error, "work_admission", "work_rejected", true),
        };
        let (reply, receiver) = oneshot::channel();
        let invocation = Invocation {
            reply_completed,
            request,
            reply,
            disconnected: disconnected.clone(),
            admission,
        };
        tokio::select! {
            biased;
            read = stream.read(&mut byte) => break 'request peer_read_outcome(read, &operation_id, "dispatch", true),
            _ = stop.cancelled() => break 'request stopping("dispatch", true),
            result = requests.send(invocation) => {
                if result.is_err() {
                    break 'request failure(ApiError::new(ErrorCode::AppNotRunning, "Owner request queue closed"), "dispatch", "request_queue_closed", true);
                }
            }
        }
        dispatched = true;
        let (command, requires_receipt) = recovery.expect("dispatched request has a command");
        let uncertain = |mut outcome: ConnectionOutcome| {
            if let ConnectionOutcome::Reply(response) = &mut outcome {
                response.error = response
                    .error
                    .take()
                    .map(|error| uncertain_reply(error, &operation_id, command, requires_receipt));
            }
            outcome
        };
        tokio::select! {
            biased;
            read = stream.read(&mut byte) => uncertain(peer_read_outcome(read, &operation_id, "await_reply", false)),
            _ = stop.cancelled() => uncertain(stopping("await_reply", false)),
            result = receiver => match result {
                Ok(mut response) => {
                    response.error = response.error.take().map(|error| ipc_failure(error, "control", "control_failure", false));
                    ConnectionOutcome::Reply(response)
                }
                Err(_) => uncertain(failure(ApiError::new(ErrorCode::IoFailed, "Operation reply unavailable"), "await_reply", "reply_sender_dropped", false)),
            },
        }
    };
    let ConnectionOutcome::Reply(response) = outcome else {
        return Ok(());
    };
    // End the observation, not an accepted mutation. The shutdown completion
    // guard still keeps Control from stopping transport before the final frame.
    disconnected.cancel();
    let bytes = match encode_frame(&response) {
        Ok(bytes) => bytes,
        Err(error) => {
            let error = ipc_failure(error, "encode_reply", "reply_encoding_failed", !dispatched);
            let error = if dispatched {
                let (command, requires_receipt) =
                    recovery.expect("dispatched request has a command");
                uncertain_reply(error, &operation_id, command, requires_receipt)
            } else {
                error
            };
            encode_frame(&Response::<Value>::failure(operation_id, error))?
        }
    };
    write_encoded_frame(&mut stream, &bytes).await?;
    // A successful write/flush does not prove the peer consumed the reply,
    // especially on a Windows named pipe. The client closes after reading it.
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            read = stream.read(&mut byte) => match read {
                Ok(0) => return Ok(()),
                Ok(_) => {},
                Err(error) => return Err(ipc_failure(ApiError::new(ErrorCode::IoFailed, format!("IPC peer close read failed: {}", error.kind())), "peer_close", "peer_read_error", false)),
            },
        }
    }
}

pub async fn call(
    data_dir: &Path,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
) -> Result<Response<Value>, ApiError> {
    let endpoint = match environment_endpoint(data_dir)? {
        Some(endpoint) => endpoint,
        None => read_endpoint(data_dir).await?,
    };
    connect(endpoint, command, operation_id, origin).await
}

/// Send a command only when the owner advertised the required capability in Welcome.
pub async fn call_requiring_capability(
    data_dir: &Path,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
    capability: &str,
) -> Result<Response<Value>, ApiError> {
    let endpoint = match environment_endpoint(data_dir)? {
        Some(endpoint) => endpoint,
        None => read_endpoint(data_dir).await?,
    };
    connect_with_capability(endpoint, command, operation_id, origin, Some(capability)).await
}

fn environment_endpoint(data_dir: &Path) -> Result<Option<Endpoint>, ApiError> {
    #[cfg(unix)]
    {
        if std::env::var_os("SHELLBOY_DATA_DIR")
            .as_deref()
            .map(Path::new)
            != Some(data_dir)
        {
            return Ok(None);
        }
        let Some(address) = std::env::var_os("SHELLBOY_SOCKET").filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let address = address.into_string().map_err(|_| endpoint_mismatch())?;
        let instance_id = std::env::var("SHELLBOY_INSTANCE_ID")
            .ok()
            .filter(|id| !id.is_empty())
            .ok_or_else(endpoint_mismatch)?;
        if !Path::new(&address).is_absolute() {
            return Err(endpoint_mismatch());
        }
        Ok(Some(Endpoint {
            protocol_version: PROTOCOL_VERSION,
            instance_id,
            transport: "unix".into(),
            address,
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = data_dir;
        Ok(None)
    }
}

async fn read_endpoint(data_dir: &Path) -> Result<Endpoint, ApiError> {
    let path = data_dir.join("endpoint.json");
    tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .map_err(io_error)?
        };
        #[cfg(not(unix))]
        let file = std::fs::File::open(path).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return Err(endpoint_mismatch());
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > 64 * 1024 {
            return Err(endpoint_mismatch());
        }
        serde_json::from_slice::<Endpoint>(&bytes).map_err(|_| endpoint_mismatch())
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Endpoint worker failed"))?
}

async fn connect(
    endpoint: Endpoint,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
) -> Result<Response<Value>, ApiError> {
    connect_with_capability(endpoint, command, operation_id, origin, None).await
}

async fn connect_with_capability(
    endpoint: Endpoint,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
    required_capability: Option<&str>,
) -> Result<Response<Value>, ApiError> {
    if endpoint.protocol_version != PROTOCOL_VERSION {
        return Err(protocol_mismatch(
            PROTOCOL_VERSION,
            endpoint.protocol_version,
        ));
    }
    #[cfg(unix)]
    {
        if endpoint.transport != "unix" {
            return Err(endpoint_mismatch());
        }
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::UnixStream::connect(&endpoint.address),
        )
        .await
        .map_err(|_| io_error(std::io::ErrorKind::TimedOut.into()))?
        .map_err(io_error)?;
        peer::verify(&stream).map_err(|_| {
            ApiError::new(
                ErrorCode::IoFailed,
                "Local IPC peer identity could not be verified",
            )
            .hint("Start Shellboy and its CLI under the same OS user")
        })?;
        call_stream_requiring_capability(
            stream,
            endpoint,
            command,
            operation_id,
            origin,
            required_capability,
        )
        .await
    }
    #[cfg(windows)]
    {
        if endpoint.transport != "pipe" {
            return Err(endpoint_mismatch());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let stream = loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&endpoint.address) {
                Ok(client) => break client,
                Err(error)
                    if error.raw_os_error() == Some(231)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await
                }
                Err(error) => return Err(io_error(error)),
            }
        };
        call_stream_requiring_capability(
            stream,
            endpoint,
            command,
            operation_id,
            origin,
            required_capability,
        )
        .await
    }
}
fn uncertain_reply(
    mut error: ApiError,
    operation_id: &str,
    command: &str,
    requires_receipt: bool,
) -> ApiError {
    let details = error.details.get_or_insert_with(|| serde_json::json!({}));
    if !details.is_object() {
        *details = serde_json::json!({"cause_details": details.take()});
    }
    details["operation_id"] = Value::String(operation_id.into());
    details["request_may_have_been_applied"] = Value::Bool(requires_receipt);
    if !requires_receipt {
        let cost = if command == "history.search" {
            " Semantic search may incur another query embedding charge."
        } else {
            ""
        };
        error.hint = Some(format!(
            "Repeat shellboy {} with the same arguments to observe fresh state; this observation does not create an operation receipt. The same operation ID also runs a new observation.{cost}",
            command.replace('.', " ")
        ));
        details["read_only"] = Value::Bool(true);
    } else {
        error.hint = Some("Use details.operation_id with shellboy operation get --operation OPERATION_ID, then inspect saved target state. A missing receipt does not prove the request was never applied; do not resubmit the mutation under a new ID".into());
        details["next_operation"] =
            serde_json::json!(Command::OperationGet(crate::protocol::OperationTarget {
                operation_id: operation_id.into()
            }));
    }
    error
}

fn handshake_timeout() -> ApiError {
    ApiError::new(ErrorCode::IoFailed,"Local IPC handshake timed out; the request was not sent")
        .hint("Check that the owner is responsive and retry the connection; do not infer a protocol version change from this timeout")
        .details(serde_json::json!({"phase":"handshake","request_may_have_been_applied":false}))
}
#[cfg(test)]
async fn call_stream<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    endpoint: Endpoint,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
) -> Result<Response<Value>, ApiError> {
    call_stream_requiring_capability(stream, endpoint, command, operation_id, origin, None).await
}

async fn call_stream_requiring_capability<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    endpoint: Endpoint,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
    required_capability: Option<&str>,
) -> Result<Response<Value>, ApiError> {
    write_frame(
        &mut stream,
        &Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let welcome: Welcome =
        tokio::time::timeout(std::time::Duration::from_secs(5), read_frame(&mut stream))
            .await
            .map_err(|_| handshake_timeout())??
            .ok_or_else(endpoint_mismatch)?;
    if welcome.protocol_version != PROTOCOL_VERSION {
        return Err(protocol_mismatch(
            PROTOCOL_VERSION,
            welcome.protocol_version,
        ));
    }
    if welcome.instance_id != endpoint.instance_id {
        return Err(endpoint_mismatch());
    }
    if let Some(capability) = required_capability
        && !welcome
            .capabilities
            .iter()
            .any(|advertised| advertised == capability)
    {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "This owner cannot resolve short IDs",
        )
        .hint("Pass the full ID or update the owner to a release advertising this capability")
        .details(serde_json::json!({
            "reason":"missing_capability",
            "required_capability":capability,
            "request_may_have_been_applied":false,
        })));
    }
    let mut request = Request::new(
        operation_id.unwrap_or_else(|| format!("{}:{}", welcome.instance_id, uuid::Uuid::new_v4())),
        command,
    );
    request.origin = origin;
    request.validate()?;
    let expected = request.operation_id.clone();
    let encoded = encode_frame(&request).map_err(|mut error| {
        error.hint = Some("The request was not sent. Reduce the input so the complete JSON request fits the frame limit, then retry".into());
        error.details = Some(serde_json::json!({
            "operation_id": expected,
            "request_may_have_been_applied": false
        }));
        error
    })?;
    let uncertain = |error| {
        uncertain_reply(
            error,
            &expected,
            request.command.name(),
            request.command.requires_receipt(),
        )
    };
    write_encoded_frame(&mut stream, &encoded)
        .await
        .map_err(uncertain)?;
    let response: Response<Value> = read_frame(&mut stream)
        .await
        .map_err(uncertain)?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::IoFailed,
                "Connection ended before reply; the operation result is unconfirmed",
            )
        })
        .map_err(uncertain)?;
    response.validate().map_err(uncertain)?;
    if response.operation_id != expected {
        return Err(uncertain(endpoint_mismatch()));
    }
    Ok(response)
}

/// An explicit data-directory shutdown never uses an inherited environment endpoint.
pub async fn shutdown_owner(
    data_dir: &Path,
    operation_id: Option<String>,
) -> Result<Response<Value>, ApiError> {
    let endpoint = read_endpoint(data_dir).await?;
    let command = Command::OwnerShutdown(crate::protocol::OwnerShutdownArgs {
        instance_id: endpoint.instance_id.clone(),
    });
    connect(endpoint, command, operation_id, None).await
}

#[cfg(test)]
mod frame_delivery_tests {
    use super::*;
    use crate::protocol::{MAX_FRAME_BYTES, MessageSendArgs};

    fn endpoint() -> Endpoint {
        Endpoint {
            protocol_version: PROTOCOL_VERSION,
            instance_id: "instance".into(),
            transport: "fixture".into(),
            address: "fixture".into(),
        }
    }

    #[tokio::test]
    async fn handshake_timeout_is_unsent_io_failure_instead_of_protocol_mismatch() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            let mut byte = [0u8; 1];
            assert_eq!(
                peer.read(&mut byte).await.unwrap(),
                0,
                "no request may be sent without a welcome"
            );
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::Status(Default::default()),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed);
        let details = error.details.unwrap();
        assert_eq!(details["phase"], "handshake");
        assert_eq!(details["request_may_have_been_applied"], false);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn envelope_overflow_is_unsent_and_exact_frame_limit_is_accepted() {
        let operation = "instance:oversized";
        let make = |text| {
            Command::MessageSend(MessageSendArgs {
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                deliver: crate::protocol::Deliver::Next,
                thread_id: Some("t".into()),
                text,
            })
        };
        let overhead = serde_json::to_vec(&Request::new(operation, make(String::new())))
            .unwrap()
            .len();
        let text = "x".repeat(MAX_FRAME_BYTES - overhead);
        let exact = Request::new(operation, make(text.clone()));
        assert_eq!(serde_json::to_vec(&exact).unwrap().len(), MAX_FRAME_BYTES);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &exact).await.unwrap();
        assert_eq!(bytes.len(), MAX_FRAME_BYTES + 4);
        assert_eq!(
            read_frame::<_, Request>(&mut bytes.as_slice())
                .await
                .unwrap()
                .unwrap(),
            exact
        );

        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            // Preflight refusal must close without sending even a request prefix.
            assert!(read_frame::<_, Request>(&mut peer).await.unwrap().is_none());
        });
        let error = call_stream(
            client,
            endpoint(),
            make(text + "x"),
            Some(operation.into()),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InputLimit);
        let details = error.details.unwrap();
        assert_eq!(details["operation_id"], operation);
        assert_eq!(details["request_may_have_been_applied"], false);
        assert!(details.get("next_operation").is_none());
        task.await.unwrap();
    }

    #[tokio::test]
    async fn lost_reply_after_sent_request_keeps_the_operation_uncertain() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
            assert_eq!(request.operation_id, "instance:sent");
            // A received request is no proof that its reply/receipt reached this client.
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::MessageSend(MessageSendArgs {
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                deliver: crate::protocol::Deliver::Next,
                thread_id: Some("t".into()),
                text: "hello".into(),
            }),
            Some("instance:sent".into()),
            None,
        )
        .await
        .unwrap_err();
        let details = error.details.unwrap();
        assert_eq!(details["operation_id"], "instance:sent");
        assert_eq!(details["request_may_have_been_applied"], true);
        assert_eq!(details["next_operation"]["command"], "operation.get");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn lost_reply_distinguishes_receipt_free_observations_and_stateful_waits() {
        let cases = [
            Command::Status(Default::default()),
            Command::ThreadGet(crate::protocol::ThreadTarget {
                thread_id: Some("t".into()),
            }),
            Command::IndexStatus(crate::protocol::IndexArgs {
                kinds: Vec::new(),
                dimensions: None,
                scope: None,
                space_id: None,
                thread_ids: Vec::new(),
                source_ids: Vec::new(),
            }),
            Command::Wait(crate::protocol::WaitArgs {
                after: None,
                conditions: vec![crate::protocol::WaitCondition::RunEnded { run_id: "r".into() }],
                timeout_ms: 0,
            }),
        ];
        for command in cases {
            let name = command.name();
            let (client, mut peer) = tokio::io::duplex(4096);
            let task = tokio::spawn(async move {
                read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
                write_frame(
                    &mut peer,
                    &Welcome {
                        protocol_version: PROTOCOL_VERSION,
                        capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                            .iter()
                            .map(|s| (*s).to_owned())
                            .collect(),
                        instance_id: "instance".into(),
                    },
                )
                .await
                .unwrap();
                let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
                assert_eq!(request.command.name(), name);
                // Drop exactly after receiving the request, without dispatching
                // any fixture mutation or assuming a receipt reached the client.
            });
            let error = call_stream(
                client,
                endpoint(),
                command,
                Some("instance:lost".into()),
                None,
            )
            .await
            .unwrap_err();
            let hint = error.hint.unwrap();
            let details = error.details.unwrap();
            assert_eq!(details["operation_id"], "instance:lost");
            if name == "wait" {
                assert_eq!(details["request_may_have_been_applied"], true);
                assert_ne!(details["read_only"], true);
                assert!(hint.contains("do not resubmit the mutation under a new ID"));
            } else {
                assert_eq!(details["read_only"], true);
                assert_eq!(details["request_may_have_been_applied"], false);
                assert!(details.get("next_operation").is_none());
                assert!(hint.contains(&name.replace('.', " ")));
                assert!(hint.contains("does not create an operation receipt"));
                assert!(hint.contains("same operation ID also runs a new observation"));
            }
            task.await.unwrap();
        }
    }

    fn assert_version_mismatch(error: &ApiError, owner: u32) {
        assert_eq!(error.code, ErrorCode::ProtocolMismatch);
        let details = error.details.as_ref().unwrap();
        assert_eq!(details["client_protocol_version"], PROTOCOL_VERSION);
        assert_eq!(details["owner_protocol_version"], owner);
        assert_eq!(details["request_may_have_been_applied"], false);
        assert!(
            error
                .message
                .contains(&format!("CLI uses protocol {PROTOCOL_VERSION}"))
        );
        assert!(error.message.contains(&format!("owner uses {owner}")));
        assert!(error.hint.as_ref().unwrap().contains("same release"));
        assert!(!error.message.contains("retry"));
    }

    #[tokio::test]
    async fn mismatched_endpoint_protocol_is_rejected_before_transport_connection() {
        let mut saved = endpoint();
        saved.protocol_version = PROTOCOL_VERSION - 1;
        // The fixture has neither a valid transport nor an address. Version
        // rejection must precede either transport validation or connection I/O.
        let error = connect(saved, Command::Status(Default::default()), None, None)
            .await
            .unwrap_err();
        assert_version_mismatch(&error, PROTOCOL_VERSION - 1);
    }

    #[tokio::test]
    async fn changed_owner_instance_is_distinct_from_protocol_mismatch_before_dispatch() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "replacement-instance".into(),
                },
            )
            .await
            .unwrap();
            assert!(read_frame::<_, Value>(&mut peer).await.unwrap().is_none());
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::Status(Default::default()),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProtocolMismatch);
        assert!(error.message.contains("owner instance changed"));
        assert!(!error.message.contains("protocol"));
        assert!(error.details.is_none());
        task.await.unwrap();
    }

    #[tokio::test]
    async fn mismatched_reply_id_after_mutation_preserves_uncertain_receipt_guidance() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Response::success("instance:wrong", serde_json::json!({})),
            )
            .await
            .unwrap();
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::MessageSend(MessageSendArgs {
                resume_recovery: false,
                human: false,
                to_human: false,
                expect_reply: false,
                sender_label: None,
                deliver: crate::protocol::Deliver::Next,
                thread_id: Some("t".into()),
                text: "hello".into(),
            }),
            Some("instance:sent".into()),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            error
                .hint
                .as_ref()
                .unwrap()
                .contains("do not resubmit the mutation under a new ID")
        );
        let details = error.details.as_ref().unwrap();
        assert_eq!(details["request_may_have_been_applied"], true);
        assert_eq!(details["operation_id"], "instance:sent");
        assert_eq!(details["next_operation"]["command"], "operation.get");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn unknown_owner_capabilities_are_ignored_and_request_continues() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: vec!["future.capability".into()],
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            let request: Request = read_frame(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Response::success(request.operation_id, serde_json::json!({})),
            )
            .await
            .unwrap();
        });
        let response = call_stream(
            client,
            endpoint(),
            Command::Status(Default::default()),
            None,
            None,
        )
        .await
        .expect("an additive capability must not block a same-version connection");
        assert!(response.ok);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn mismatched_owner_protocol_is_rejected_before_sending_command() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let hello = read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION - 1,
                    capabilities: Vec::new(),
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            assert!(read_frame::<_, Value>(&mut peer).await.unwrap().is_none());
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::ThreadList(crate::protocol::ThreadListArgs {
                limit: 1,
                cursor: None,
                query: None,
                awaiting: None,
                archived: Default::default(),
                tags: Vec::new(),
                cwd: None,
            }),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProtocolMismatch);
        assert_version_mismatch(&error, PROTOCOL_VERSION - 1);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_request_frames_close_without_dispatch_or_a_fabricated_reply() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for bytes in [
            vec![0, 0, 0, 1, b'{'],
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 8, b'{'],
            ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec(),
        ] {
            let (mut client, server) = tokio::io::duplex(4096);
            let (requests, mut incoming) = mpsc::channel(1);
            let task = tokio::spawn(serve_connection(
                server,
                "instance".into(),
                requests,
                CancellationToken::new(),
                server::WorkAdmission::new(),
            ));
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
            client.write_all(&bytes).await.unwrap();
            client.shutdown().await.unwrap();
            assert!(task.await.unwrap().is_err());
            assert!(incoming.recv().await.is_none());
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            assert!(response.is_empty());
        }
    }
    #[tokio::test]
    async fn decoded_invalid_request_gets_its_own_id_and_closes_without_dispatch() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (requests, mut incoming) = mpsc::channel(1);
        let task = tokio::spawn(serve_connection(
            server,
            "instance".into(),
            requests,
            CancellationToken::new(),
            server::WorkAdmission::new(),
        ));
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
        let command = Command::ThreadList(crate::protocol::ThreadListArgs {
            limit: 0,
            ..Default::default()
        });
        write_frame(&mut client, &Request::new("instance:invalid", command))
            .await
            .unwrap();
        let response: Response<Value> = read_frame(&mut client).await.unwrap().unwrap();
        response.validate().unwrap();
        assert_eq!(response.operation_id, "instance:invalid");
        assert_eq!(response.error.unwrap().code, ErrorCode::InvalidArgument);
        drop(client);
        task.await.unwrap().unwrap();
        assert!(incoming.recv().await.is_none());
    }
    async fn rejected_reply(frame: Value) -> (Response<Value>, bool) {
        let (mut client, server) = tokio::io::duplex(4096);
        let (requests, mut incoming) = mpsc::channel(1);
        let task = tokio::spawn(serve_connection(
            server,
            "instance".into(),
            requests,
            CancellationToken::new(),
            server::WorkAdmission::new(),
        ));
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
        write_frame(&mut client, &frame).await.unwrap();
        let response: Response<Value> = read_frame(&mut client).await.unwrap().unwrap();
        response.validate().unwrap();
        drop(client);
        task.await.unwrap().unwrap();
        let dispatched = incoming.recv().await.is_some();
        (response, dispatched)
    }
    #[tokio::test]
    async fn unknown_command_field_or_type_gets_a_typed_reply_without_dispatch() {
        let base = |extra: Value| {
            let mut frame = serde_json::json!({
                "protocol_version": PROTOCOL_VERSION,
                "operation_id": "instance:typed"
            });
            for (key, value) in extra.as_object().unwrap() {
                frame[key] = value.clone();
            }
            frame
        };
        let cases = [
            (
                base(serde_json::json!({"command": "future.thing", "args": {}})),
                "unknown_command",
                "unknown command `future.thing`",
            ),
            (
                base(serde_json::json!({"command": "message.send", "args": {
                    "text": "hi", "thread_id": "t", "human": false, "from_the_future": 1}})),
                "unknown_field",
                "unknown field `from_the_future` in the arguments of `message.send`",
            ),
            (
                base(serde_json::json!({"command": "status", "args": {}, "extra": 1})),
                "unknown_request_field",
                "unknown request field `extra`",
            ),
            (
                base(serde_json::json!({"command": "message.send", "args": {
                    "text": 5, "thread_id": "t"}})),
                "invalid_shape",
                "invalid arguments for `message.send`",
            ),
        ];
        for (frame, reason, message) in cases {
            let (response, dispatched) = rejected_reply(frame).await;
            assert!(!dispatched, "{reason} must not reach the dispatcher");
            assert_eq!(response.operation_id, "instance:typed");
            let error = response.error.unwrap();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{reason}");
            assert!(error.message.starts_with(message), "{}", error.message);
            let details = error.details.unwrap();
            assert_eq!(details["reason"], reason);
            assert_eq!(details["request_may_have_been_applied"], false);
            assert!(!error.message.contains('5'), "values must not be echoed");
        }
    }
    #[tokio::test]
    async fn typed_rejection_of_a_mutation_is_not_reported_as_uncertain() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let owner = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            let frame: Value = read_frame(&mut peer).await.unwrap().unwrap();
            let id = frame["operation_id"].as_str().unwrap().to_owned();
            let mut frame = frame;
            frame["args"]["human"] = Value::Bool(true);
            frame["args"]["unknown_to_owner"] = Value::Bool(true);
            let (_, error) = Request::decode_frame(frame).unwrap_err();
            write_frame(&mut peer, &Response::<Value>::failure(id, error))
                .await
                .unwrap();
        });
        let command = Command::MessageSend(MessageSendArgs {
            resume_recovery: false,
            human: true,
            to_human: false,
            expect_reply: false,
            sender_label: None,
            deliver: crate::protocol::Deliver::Next,
            thread_id: Some("t".into()),
            text: "hello".into(),
        });
        let response = call_stream(client, endpoint(), command, None, None)
            .await
            .unwrap();
        let error = response.error.unwrap();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        let details = error.details.unwrap();
        assert_eq!(details["request_may_have_been_applied"], false);
        assert!(details.get("next_operation").is_none());
        owner.await.unwrap();
    }
    async fn send_test_frame(client: &mut tokio::io::DuplexStream, frame: &Value) {
        write_frame(
            client,
            &Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        read_frame::<_, Welcome>(client).await.unwrap().unwrap();
        write_frame(client, frame).await.unwrap();
    }

    fn status_frame() -> Value {
        serde_json::to_value(Request::new(
            "instance:test",
            Command::Status(Default::default()),
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn owner_close_after_request_returns_typed_failure() {
        crate::test_watchdog::watchdog("IPC final failure frame", async {
            for (case, stage, reason, code) in [
                ("decode", "decode", "unknown_command", ErrorCode::InvalidArgument),
                ("validation", "validate", "invalid_request", ErrorCode::InvalidArgument),
                ("admission", "work_admission", "work_rejected", ErrorCode::InvalidState),
                ("control", "control", "control_failure", ErrorCode::InvalidState),
                ("closed", "dispatch", "request_queue_closed", ErrorCode::AppNotRunning),
                ("dropped", "await_reply", "reply_sender_dropped", ErrorCode::IoFailed),
                ("stop", "await_reply", "transport_stopping", ErrorCode::AppNotRunning),
                ("oversize", "encode_reply", "reply_encoding_failed", ErrorCode::InputLimit),
            ] {
                let (mut client, server_stream) = tokio::io::duplex(4096);
                let (requests, mut incoming) = mpsc::channel(1);
                if case == "closed" { incoming.close(); }
                let stop = CancellationToken::new();
                let work = server::WorkAdmission::new();
                let mut permits = Vec::new();
                if case == "admission" {
                    let status = Command::Status(Default::default());
                    while let Ok(permit) = work.try_acquire(&status) { permits.push(permit); }
                }
                let task = tokio::spawn(serve_connection(server_stream, "instance".into(), requests, stop.clone(), work));
                let frame = match case {
                    "decode" => serde_json::json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"instance:test","command":"no.such.command","args":{}}),
                    "validation" => serde_json::to_value(Request::new("instance:test", Command::ThreadList(crate::protocol::ThreadListArgs { limit: 0, ..Default::default() }))).unwrap(),
                    _ => status_frame(),
                };
                send_test_frame(&mut client, &frame).await;
                let mut held = None;
                match case {
                    "control" | "oversize" => {
                        let invocation = incoming.recv().await.unwrap();
                        let id = invocation.request.operation_id;
                        let response = if case == "control" {
                            Response::failure(id, ApiError::new(ErrorCode::InvalidState, "admission refused"))
                        } else {
                            Response::success(id, Value::String("x".repeat(MAX_FRAME_BYTES)))
                        };
                        invocation.reply.send(response).unwrap();
                    }
                    "dropped" => drop(incoming.recv().await.unwrap()),
                    "stop" => { held = incoming.recv().await; assert!(held.is_some()); stop.cancel(); }
                    _ => {}
                }
                let response = read_frame::<_, Response<Value>>(&mut client).await.unwrap().expect(case);
                response.validate().unwrap();
                assert_eq!(response.operation_id, "instance:test", "{case}");
                let error = response.error.unwrap();
                assert_eq!(error.code, code, "{case}");
                let details = error.details.unwrap();
                assert_eq!(details["stage"], stage, "{case}");
                assert_eq!(details["reason"], reason, "{case}");
                drop(client);
                task.await.unwrap().unwrap();
                drop(held);
            }
        }).await.unwrap();
    }

    #[tokio::test]
    async fn clean_peer_eof_cancels_without_reply() {
        use tokio::io::AsyncWriteExt;
        crate::test_watchdog::watchdog("IPC clean EOF", async {
            let (mut client, stream) = tokio::io::duplex(4096);
            let (requests, mut incoming) = mpsc::channel(1);
            let task = tokio::spawn(serve_connection(
                stream,
                "instance".into(),
                requests,
                CancellationToken::new(),
                server::WorkAdmission::new(),
            ));
            send_test_frame(&mut client, &status_frame()).await;
            let invocation = incoming.recv().await.unwrap();
            client.shutdown().await.unwrap();
            invocation.disconnected.cancelled().await;
            assert!(
                read_frame::<_, Response<Value>>(&mut client)
                    .await
                    .unwrap()
                    .is_none()
            );
            task.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }

    struct FailPeerRead {
        stream: tokio::io::DuplexStream,
        fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl AsyncRead for FailPeerRead {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let mut probe = [0];
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                // The trigger byte wakes an already-pending read; replace only
                // that read's result, leaving the connection writable.
                let mut read = tokio::io::ReadBuf::new(&mut probe);
                return match std::pin::Pin::new(&mut self.stream).poll_read(cx, &mut read) {
                    std::task::Poll::Ready(Ok(())) if !read.filled().is_empty() => {
                        self.fail.store(false, std::sync::atomic::Ordering::SeqCst);
                        std::task::Poll::Ready(Err(std::io::ErrorKind::Other.into()))
                    }
                    result => result,
                };
            }
            std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for FailPeerRead {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.stream).poll_write(cx, bytes)
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.stream).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn extra_byte_or_peer_read_error_gets_typed_reply() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use tokio::io::AsyncWriteExt;
        crate::test_watchdog::watchdog("IPC non-EOF peer input", async {
            for inject_error in [false, true] {
                let (mut client, stream) = tokio::io::duplex(4096);
                let fail = Arc::new(AtomicBool::new(false));
                let (requests, mut incoming) = mpsc::channel(1);
                let task = tokio::spawn(serve_connection(
                    FailPeerRead {
                        stream,
                        fail: fail.clone(),
                    },
                    "instance".into(),
                    requests,
                    CancellationToken::new(),
                    server::WorkAdmission::new(),
                ));
                send_test_frame(&mut client, &status_frame()).await;
                let invocation = incoming.recv().await.unwrap();
                fail.store(inject_error, Ordering::SeqCst);
                client.write_all(b"x").await.unwrap();
                let response = read_frame::<_, Response<Value>>(&mut client)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(response.operation_id, "instance:test");
                let error = response.error.unwrap();
                let details = error.details.unwrap();
                assert_eq!(details["stage"], "await_reply");
                assert_eq!(
                    details["reason"],
                    if inject_error {
                        "peer_read_error"
                    } else {
                        "unexpected_peer_byte"
                    }
                );
                drop(client);
                task.await.unwrap().unwrap();
                assert!(invocation.disconnected.is_cancelled());
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn final_frame_keeps_connection_until_peer_closes() {
        crate::test_watchdog::watchdog("IPC final frame lifetime", async {
            for (reject, explicit_stop) in [(false, false), (true, false), (false, true)] {
                let (mut client, stream) = tokio::io::duplex(4096);
                let (requests, mut incoming) = mpsc::channel(1);
                let stop = CancellationToken::new();
                let mut task = tokio::spawn(serve_connection(
                    stream,
                    "instance".into(),
                    requests,
                    stop.clone(),
                    server::WorkAdmission::new(),
                ));
                let mut frame = status_frame();
                if reject {
                    frame["command"] = Value::String("no.such.command".into());
                }
                send_test_frame(&mut client, &frame).await;
                if !reject {
                    let invocation = incoming.recv().await.unwrap();
                    invocation
                        .reply
                        .send(Response::success(
                            invocation.request.operation_id,
                            Value::Bool(true),
                        ))
                        .unwrap();
                }
                let response = read_frame::<_, Response<Value>>(&mut client)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(response.ok, !reject);
                assert!(
                    futures_util::poll!(&mut task).is_pending(),
                    "owner dropped stream before peer close"
                );
                if explicit_stop {
                    stop.cancel();
                    task.await.unwrap().unwrap();
                    assert!(read_frame::<_, Value>(&mut client).await.unwrap().is_none());
                } else {
                    drop(client);
                    task.await.unwrap().unwrap();
                }
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn missing_capability_rejects_prefix_query_before_sending_a_request() {
        use crate::protocol::{IdKind, IdResolveArgs};

        let (mut server, client) = tokio::io::duplex(4096);
        let server_task = tokio::spawn(async move {
            let hello: Hello = read_frame(&mut server).await.unwrap().unwrap();
            assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
            write_frame(
                &mut server,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: "instance".into(),
                    capabilities: Vec::new(),
                },
            )
            .await
            .unwrap();
            let mut byte = [0u8; 1];
            assert_eq!(
                server.read(&mut byte).await.unwrap(),
                0,
                "no id.resolve request is sent without capability"
            );
        });
        let error = call_stream_requiring_capability(
            client,
            endpoint(),
            Command::IdResolve(IdResolveArgs {
                kind: IdKind::Thread,
                prefix: "12345678".into(),
                include_pending_duties: false,
            }),
            None,
            None,
            Some("id.resolve"),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(
            error.details.as_ref().unwrap()["required_capability"],
            "id.resolve"
        );
        assert_eq!(
            error.details.as_ref().unwrap()["request_may_have_been_applied"],
            false
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn partial_request_prefix_and_payload_time_out_without_dispatch() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let partials = [
            vec![0, 0],
            [(MAX_FRAME_BYTES as u32).to_be_bytes().as_slice(), b"{"].concat(),
        ];
        let mut cases = tokio::task::JoinSet::new();
        for bytes in partials {
            cases.spawn(async move {
                let (mut client, server) = tokio::io::duplex(4096);
                let (requests, mut incoming) = mpsc::channel(1);
                let task = tokio::spawn(serve_connection(
                    server,
                    "instance".into(),
                    requests,
                    CancellationToken::new(),
                    server::WorkAdmission::new(),
                ));
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
                client.write_all(&bytes).await.unwrap();
                let error = crate::test_watchdog::watchdog(
                    "src/ipc.rs: partial_request_prefix_and_payload_time_out_without_dispatch",
                    task,
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
                assert_eq!(error.code, ErrorCode::IoFailed);
                assert!(error.message.contains("timed out"));
                assert!(incoming.recv().await.is_none());
                let mut response = Vec::new();
                client.read_to_end(&mut response).await.unwrap();
                assert!(response.is_empty());
            });
        }
        while let Some(result) = cases.join_next().await {
            result.unwrap();
        }
    }
}
