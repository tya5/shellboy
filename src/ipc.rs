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
#[cfg(unix)]
pub(crate) use server::MAX_CONNECTIONS;
#[cfg(test)]
pub(crate) use server::tests::Fixture as TestServer;
pub(crate) use server::{AdmissionRefusals, MAX_WAIT_WORK, is_wait};

const FRAME_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct Hello {
    pub protocol_version: u32,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
pub struct Welcome {
    pub protocol_version: u32,
    pub instance_id: String,
    /// Prefix that new operation IDs carry; read at each handshake (#450).
    pub operation_epoch: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "decode_owner_version"
    )]
    #[schemars(length(max = 128))]
    pub owner_version: Option<String>,
}

const OWNER_VERSION_MAX_BYTES: usize = 128;

fn valid_build_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_owner_version(value: &str) -> bool {
    if value.len() > OWNER_VERSION_MAX_BYTES || !value.is_ascii() {
        return false;
    }
    let (version, sha) = value
        .split_once(' ')
        .map_or((value, None), |(version, sha)| (version, Some(sha)));
    version.parse::<semver::Version>().is_ok() && sha.is_none_or(valid_build_sha)
}

pub(crate) fn decode_owner_version<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<Option<String>, D::Error> {
    // Optional metadata must not prevent an otherwise compatible connection.
    Ok(match Value::deserialize(input)? {
        Value::String(value) if valid_owner_version(&value) => Some(value),
        _ => None,
    })
}

fn build_owner_version(sha: Option<&str>) -> Option<String> {
    let version = env!("CARGO_PKG_VERSION");
    let value = match sha.filter(|sha| valid_build_sha(sha)) {
        Some(sha) => format!("{version} {sha}"),
        None => version.to_owned(),
    };
    valid_owner_version(&value).then_some(value)
}
pub(crate) fn current_owner_version() -> Option<String> {
    build_owner_version(option_env!("SHELLBOY_BUILD_GIT_SHA"))
}

pub struct Invocation {
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

fn peer_read_outcome(read: std::io::Result<usize>, operation_id: &str) -> ConnectionOutcome {
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
        ipc_failure(error, "await_reply", reason, false),
    ))
}

async fn serve_connection<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    epoch: std::sync::Arc<crate::control::OperationEpoch>,
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
                owner_version: current_owner_version(),
                protocol_version: PROTOCOL_VERSION,
                capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                instance_id: epoch.instance().to_owned(),
                operation_epoch: epoch.current(),
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
    let failure = |error, stage, reason, unapplied| {
        ConnectionOutcome::Reply(Response::failure(
            operation_id.clone(),
            ipc_failure(error, stage, reason, unapplied),
        ))
    };
    let recovery = request.as_ref().ok().map(|request| {
        (
            request.command.name(),
            request.command.is_read_only(),
            request.command.requires_receipt(),
        )
    });
    let mut dispatched = false;
    let mut refused_wait = false;
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
            Err(error) => {
                refused_wait = server::is_wait(&request.command);
                break 'request failure(error, "work_admission", "work_rejected", true);
            }
        };
        let (reply, receiver) = oneshot::channel();
        let invocation = Invocation {
            request,
            reply,
            disconnected: disconnected.clone(),
            admission,
        };
        // Dispatch the complete request before watching for peer EOF. Disconnect
        // cancels observation, not the delivery of an already-read request.
        if requests.send(invocation).await.is_err() {
            break 'request failure(
                ApiError::new(ErrorCode::AppNotRunning, "Owner request queue closed"),
                "dispatch",
                "request_queue_closed",
                true,
            );
        }
        dispatched = true;
        let (command, read_only, requires_receipt) =
            recovery.expect("dispatched request has a command");
        let uncertain = |mut outcome: ConnectionOutcome| {
            if let ConnectionOutcome::Reply(response) = &mut outcome {
                response.error = response.error.take().map(|error| {
                    uncertain_reply(error, &operation_id, command, read_only, requires_receipt)
                });
            }
            outcome
        };
        tokio::select! {
            biased;
            read = stream.read(&mut byte) => uncertain(peer_read_outcome(read, &operation_id)),
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
    disconnected.cancel();
    let bytes = match encode_frame(&response) {
        Ok(bytes) => bytes,
        Err(error) => {
            let error = ipc_failure(error, "encode_reply", "reply_encoding_failed", !dispatched);
            let error = if dispatched {
                let (command, read_only, requires_receipt) =
                    recovery.expect("dispatched request has a command");
                uncertain_reply(error, &operation_id, command, read_only, requires_receipt)
            } else {
                error
            };
            encode_frame(&Response::<Value>::failure(operation_id, error))?
        }
    };
    {
        let write = write_encoded_frame(&mut stream, &bytes);
        tokio::pin!(write);
        tokio::select! {
            result = &mut write => result?,
            _ = tokio::time::sleep(FRAME_READ_TIMEOUT) => return Ok(()),
            _ = stop.cancelled() => {
                // At the delivery cutoff, finish only what can complete now.
                match futures_util::FutureExt::now_or_never(write) {
                    Some(result) => result?,
                    None => return Ok(()),
                }
            }
        }
    }
    if refused_wait {
        // No invocation owns this connection: release it after the refusal frame.
        return Ok(());
    }
    // A successful write/flush does not prove the peer consumed the reply,
    // especially on a Windows named pipe. The client closes after reading it.
    let wait_for_close = async {
        loop {
            match stream.read(&mut byte).await {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error) => {
                    return Err(ipc_failure(
                        ApiError::new(
                            ErrorCode::IoFailed,
                            format!("IPC peer close read failed: {}", error.kind()),
                        ),
                        "peer_close",
                        "peer_read_error",
                        false,
                    ));
                }
            }
        }
    };
    tokio::time::timeout(FRAME_READ_TIMEOUT, wait_for_close)
        .await
        .unwrap_or(Ok(()))
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

/// A connected composer never addresses a replacement owner implicitly.
pub(crate) async fn call_selected_operation(
    data_dir: &Path,
    instance: &str,
    command: Command,
    operation_id: Option<String>,
) -> Result<Response<Value>, ApiError> {
    call_selected(data_dir, instance, command, operation_id, None).await
}

pub(crate) async fn call_selected(
    data_dir: &Path,
    instance: &str,
    command: Command,
    operation_id: Option<String>,
    announce: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<Response<Value>, ApiError> {
    let endpoint = read_endpoint(data_dir).await?;
    if endpoint.instance_id != instance {
        return Err(endpoint_mismatch());
    }
    connect_with_capability(endpoint, command, operation_id, None, None, announce).await
}

/// The CLI retries only a definite wait admission refusal, on the same owner.
pub async fn call_cli_wait(
    data_dir: &Path,
    args: crate::protocol::WaitArgs,
    operation_id: Option<String>,
    origin: Option<Origin>,
) -> Result<Response<Value>, ApiError> {
    let endpoint = match environment_endpoint(data_dir)? {
        Some(endpoint) => endpoint,
        None => read_endpoint(data_dir).await?,
    };
    // Without a caller ID the first handshake mints one in the owner's current
    // operation epoch; definite admission refusals reuse that ID.
    retry_cli_wait(args, operation_id, |args, operation| {
        connect(
            endpoint.clone(),
            Command::Wait(args),
            operation,
            origin.clone(),
        )
    })
    .await
}

async fn retry_cli_wait<F, Fut>(
    mut args: crate::protocol::WaitArgs,
    mut operation: Option<String>,
    mut attempt: F,
) -> Result<Response<Value>, ApiError>
where
    F: FnMut(crate::protocol::WaitArgs, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Response<Value>, ApiError>>,
{
    use std::time::Duration;
    use tokio::time::{Instant, sleep};

    let budget = Duration::from_millis(args.timeout_ms);
    let started = Instant::now();
    let mut backoff = server::ACCEPT_BACKOFF;
    let mut refusals = 0_u64;
    loop {
        let elapsed = started.elapsed();
        let remaining = budget.saturating_sub(elapsed);
        if let (Some(operation), true) = (&operation, refusals != 0 && remaining.is_zero()) {
            let result = crate::protocol::WaitResult {
                status: crate::protocol::WaitStatus::Timeout,
                matched: Vec::new(),
                snapshot: serde_json::json!({"conditions": []}),
                cursor: args.after,
                reason: Some(crate::protocol::wait_timeout_reason(
                    budget.as_millis() as u64
                )),
                hint: None,
                admission_wait_ms: Some(budget.as_millis() as u64),
                admission_refusals: Some(refusals),
            };
            return Ok(Response::success(operation, serde_json::json!(result)));
        }
        let admission_wait_ms = if refusals == 0 {
            0
        } else {
            elapsed.as_millis() as u64
        };
        if refusals != 0 {
            args.timeout_ms = remaining.as_millis() as u64;
        }
        let mut response = attempt(args.clone(), operation.clone()).await?;
        operation.get_or_insert_with(|| response.operation_id.clone());
        let refused = response.error.as_ref().is_some_and(|error| {
            error.code == ErrorCode::InvalidState
                && error.details.as_ref().is_some_and(|details| {
                    details["phase"] == "admission"
                        && details["request_may_have_been_applied"] == false
                })
        });
        if !refused {
            if let Some(Value::Object(result)) = response.result.as_mut() {
                result.insert("admission_wait_ms".into(), admission_wait_ms.into());
                result.insert("admission_refusals".into(), refusals.into());
            }
            return Ok(response);
        }
        refusals += 1;
        sleep(backoff.min(budget.saturating_sub(started.elapsed()))).await;
        backoff = (backoff * 2).min(server::MAX_ACCEPT_BACKOFF);
    }
}

/// One request whose ID the handshake mints in the owner's current operation
/// epoch; `announce` receives that ID before the request is sent.
pub async fn call_announcing(
    data_dir: &Path,
    command: Command,
    origin: Option<Origin>,
    announce: &mut (dyn FnMut(&str) + Send),
) -> Result<Response<Value>, ApiError> {
    let endpoint = match environment_endpoint(data_dir)? {
        Some(endpoint) => endpoint,
        None => read_endpoint(data_dir).await?,
    };
    connect_with_capability(endpoint, command, None, origin, None, Some(announce)).await
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
    connect_with_capability(
        endpoint,
        command,
        operation_id,
        origin,
        Some(capability),
        None,
    )
    .await
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
            owner_version: None,
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
    read_endpoint_if_present(data_dir)
        .await?
        .ok_or_else(|| io_error(std::io::ErrorKind::NotFound.into()))
}

async fn read_endpoint_if_present(data_dir: &Path) -> Result<Option<Endpoint>, ApiError> {
    let path = data_dir.join("endpoint.json");
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(endpoint_mismatch()),
        };
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
        serde_json::from_slice::<Endpoint>(&bytes)
            .map(Some)
            .map_err(|_| endpoint_mismatch())
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
    connect_with_capability(endpoint, command, operation_id, origin, None, None).await
}

async fn connect_with_capability(
    endpoint: Endpoint,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
    required_capability: Option<&str>,
    announce: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<Response<Value>, ApiError> {
    let stream = open_transport(&endpoint, true)
        .await?
        .ok_or_else(|| io_error(std::io::ErrorKind::NotFound.into()))?;
    call_stream_requiring_capability(
        stream,
        endpoint,
        command,
        operation_id,
        origin,
        required_capability,
        announce,
    )
    .await
}

pub(crate) async fn probe_selected(data_dir: &Path) -> Result<Option<Welcome>, ApiError> {
    let Some(endpoint) = read_endpoint_if_present(data_dir).await? else {
        return Ok(None);
    };
    let Some(mut stream) = open_transport(&endpoint, false).await? else {
        return Ok(None);
    };
    exchange_hello(&mut stream, &endpoint).await.map(Some)
}

#[cfg(unix)]
type NativeStream = tokio::net::UnixStream;
#[cfg(windows)]
type NativeStream = tokio::net::windows::named_pipe::NamedPipeClient;

async fn open_transport(
    endpoint: &Endpoint,
    wait_pipe: bool,
) -> Result<Option<NativeStream>, ApiError> {
    if endpoint.protocol_version != PROTOCOL_VERSION {
        let mut error = protocol_mismatch(PROTOCOL_VERSION, endpoint.protocol_version);
        if let Some(version) = endpoint.owner_version.clone()
            && let Some(details) = error.details.as_mut()
        {
            details["owner_version"] = Value::String(version);
        }
        return Err(error);
    }
    fn open_error(error: std::io::Error) -> Result<Option<NativeStream>, ApiError> {
        match error.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Ok(None),
            _ => Err(io_error(error)),
        }
    }
    #[cfg(unix)]
    {
        let _ = wait_pipe;
        if endpoint.transport != "unix" {
            return Err(endpoint_mismatch());
        }
        let stream = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::UnixStream::connect(&endpoint.address),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => return open_error(error),
            Err(_) => return Err(io_error(std::io::ErrorKind::TimedOut.into())),
        };
        peer::verify(&stream).map_err(|_| {
            ApiError::new(
                ErrorCode::IoFailed,
                "Local IPC peer identity could not be verified",
            )
            .hint("Start Shellboy and its CLI under the same OS user")
        })?;
        Ok(Some(stream))
    }
    #[cfg(windows)]
    {
        if endpoint.transport != "pipe" {
            return Err(endpoint_mismatch());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&endpoint.address) {
                Ok(client) => return Ok(Some(client)),
                Err(error)
                    if wait_pipe
                        && error.raw_os_error() == Some(231)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => return open_error(error),
            }
        }
    }
}
fn uncertain_reply(
    mut error: ApiError,
    operation_id: &str,
    command: &str,
    read_only: bool,
    requires_receipt: bool,
) -> ApiError {
    let details = error.details.get_or_insert_with(|| serde_json::json!({}));
    if !details.is_object() {
        *details = serde_json::json!({"cause_details": details.take()});
    }
    details["operation_id"] = Value::String(operation_id.into());
    details["request_may_have_been_applied"] = Value::Bool(!read_only);
    details["read_only"] = Value::Bool(read_only);
    if read_only {
        let cost = if command == "history.search" {
            " Semantic search may incur another query embedding charge."
        } else {
            ""
        };
        error.hint = Some(format!(
            "Repeat shellboy {} with the same arguments to observe fresh state; this observation does not create an operation receipt. The same operation ID also runs a new observation.{cost}",
            command.replace('.', " ")
        ));
    } else if requires_receipt {
        error.hint = Some("Follow details.next_operation: run shellboy operation get --operation OPERATION_ID with the supplied ID against the same owner data directory, then inspect the saved target state. A missing receipt does not prove the request was never applied; do not resubmit the mutation under a new ID".into());
        details["next_operation"] =
            serde_json::json!(Command::OperationGet(crate::protocol::OperationTarget {
                operation_id: operation_id.into()
            }));
    } else {
        error.hint = Some("This draft image write has no receipt. To recover an unfinished upload, explicitly begin a replacement for the same draft; do not replay an uncertain chunk".into());
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
    call_stream_requiring_capability(stream, endpoint, command, operation_id, origin, None, None)
        .await
}

async fn exchange_hello<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    endpoint: &Endpoint,
) -> Result<Welcome, ApiError> {
    write_frame(
        stream,
        &Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let welcome: Welcome =
        tokio::time::timeout(std::time::Duration::from_secs(5), read_frame(stream))
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
    Ok(welcome)
}

async fn call_stream_requiring_capability<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    endpoint: Endpoint,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
    required_capability: Option<&str>,
    announce: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<Response<Value>, ApiError> {
    let welcome = exchange_hello(&mut stream, &endpoint).await?;
    let required_capability = required_capability
        .or_else(|| matches!(command, Command::OperationCancel(_)).then_some("operation.cancel"));
    if let Some(capability) = required_capability
        && !welcome
            .capabilities
            .iter()
            .any(|advertised| advertised == capability)
    {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            if capability == "id.resolve" {
                "This owner cannot resolve short IDs"
            } else {
                "This owner cannot cancel observations by operation ID"
            },
        )
        .hint(if capability == "id.resolve" {
            "Pass the full ID or update the owner to a release advertising this capability"
        } else {
            "Use an owner advertising operation.cancel"
        })
        .details(serde_json::json!({
            "reason":"missing_capability",
            "required_capability":capability,
            "request_may_have_been_applied":false,
        })));
    }
    let mut request = Request::new(
        operation_id.unwrap_or_else(|| crate::control::mint_operation_id(&welcome.operation_epoch)),
        command,
    );
    request.origin = origin;
    request.validate()?;
    let expected = request.operation_id.clone();
    // The caller learns the chosen ID before anything is sent, so its progress
    // names the request even if the reply never arrives (#450).
    if let Some(announce) = announce {
        announce(&expected);
    }
    let encoded = encode_frame(&request).map_err(|mut error| {
        if matches!(request.command, Command::ThreadImport(_)) && error.code == ErrorCode::InputLimit {
            error = crate::thread_import::too_large();
        }
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
            request.command.is_read_only(),
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
            owner_version: None,
            protocol_version: PROTOCOL_VERSION,
            instance_id: "instance".into(),
            transport: "fixture".into(),
            address: "fixture".into(),
        }
    }

    #[tokio::test]
    async fn welcome_owner_version_reports_the_running_build() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (requests, _incoming) = mpsc::channel(1);
        let task = tokio::spawn(serve_connection(
            server,
            crate::control::OperationEpoch::new("version-owner"),
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
        let welcome = read_frame::<_, Welcome>(&mut client)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(welcome.instance_id, "version-owner");
        assert_eq!(welcome.operation_epoch, "version-owner.0");
        assert_eq!(
            welcome.owner_version,
            build_owner_version(option_env!("SHELLBOY_BUILD_GIT_SHA"))
        );
        assert!(
            welcome
                .owner_version
                .unwrap()
                .starts_with(env!("CARGO_PKG_VERSION"))
        );
        drop(client);
        let _ = task.await.unwrap();

        let package = env!("CARGO_PKG_VERSION");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            build_owner_version(Some(sha)),
            Some(format!("{package} {sha}"))
        );
        for missing in [None, Some("0123456"), Some("not-a-build-sha")] {
            assert_eq!(build_owner_version(missing).as_deref(), Some(package));
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                    operation_epoch: "instance.0".into(),
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
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                    operation_epoch: "instance.0".into(),
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
    async fn accepted_draft_with_lost_reply_is_reconciled_without_a_second_message() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\nresponse_max_retries=0\n").unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = crate::control::Control::new(&owner, root.path().into());
        let mut selected = endpoint();
        selected.instance_id.clone_from(&control.store.instance_id);
        let operation = format!("{}:lost-draft", control.store.operation_epoch.current());
        let submit = Command::DraftSubmit(Box::new(crate::protocol::DraftSubmitArgs {
            thread_id: None,
            expected_thread_revision: None,
            expected_source_message: None,
            create: crate::protocol::ThreadCreateArgs {
                agents: None,
                connection: None,
                cwd: None,
                model: None,
                effort: None,
                parent_thread_id: None,
            },
            draft: Box::new(crate::draft::DraftSnapshot {
                revision: 1,
                text: "draft kept by the client".into(),
                ..Default::default()
            }),
            deliver: crate::protocol::Deliver::Next,
        }));
        let (client, mut peer) = tokio::io::duplex(4096);
        let backend = control.clone();
        let server = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: Vec::new(),
                    instance_id: backend.store.instance_id.clone(),
                    operation_epoch: backend.store.operation_epoch.current(),
                },
            )
            .await
            .unwrap();
            let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
            let accepted = backend.request(request).await;
            assert!(accepted.ok, "{:?}", accepted.error);
            // Close the transport after commit, without handing the reply to the client.
            accepted.result.unwrap()
        });
        let error = call_stream(
            client,
            selected,
            submit.clone(),
            Some(operation.clone()),
            None,
        )
        .await
        .unwrap_err();
        let details = error.details.unwrap();
        assert_eq!(details["request_may_have_been_applied"], true);
        assert_eq!(details["operation_id"], operation);
        let accepted = server.await.unwrap();
        let lookup = serde_json::from_value::<Command>(details["next_operation"].clone()).unwrap();
        let readback = control.request(Request::new("readback", lookup)).await;
        let evidence: crate::protocol::OperationReceiptDetails =
            serde_json::from_value(readback.result.unwrap()).unwrap();
        assert_eq!(evidence.operation_id, operation);
        assert!(evidence.current_instance);
        assert_eq!(
            evidence.outcome["result"]["message_id"],
            accepted["message_id"]
        );
        let counts = |operation: String| {
            control.store.database.call(move |db| {
                Ok((
                    db.query_row(
                        "SELECT count(*) FROM operation_receipt WHERE operation_id=?1",
                        [&operation],
                        |r| r.get::<_, i64>(0),
                    )?,
                    db.query_row(
                        "SELECT count(*) FROM receipt WHERE client_operation_id=?1",
                        [&operation],
                        |r| r.get::<_, i64>(0),
                    )?,
                ))
            })
        };
        assert_eq!(counts(operation.clone()).await.unwrap(), (1, 1));
        assert!(
            control
                .request(Request::new(
                    "verify:no-query-receipt",
                    Command::OperationGet(crate::protocol::OperationTarget {
                        operation_id: "readback".into()
                    })
                ))
                .await
                .error
                .is_some_and(|e| e.code == ErrorCode::OperationNotFound)
        );
        // Reconcile by lookup first; deliberate same-ID replay remains idempotent.
        let replay = control
            .request(Request::new(operation.clone(), submit))
            .await;
        assert!(replay.ok, "{:?}", replay.error);
        assert_eq!(replay.result.unwrap(), accepted);
        assert_eq!(counts(operation).await.unwrap(), (1, 1));
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn lost_reply_distinguishes_receipt_free_observations_and_stateful_waits() {
        let cases = [
            Command::Status(Default::default()),
            Command::ThreadGet(crate::protocol::ThreadGetArgs {
                thread_id: Some("t".into()),
                display: false,
            }),
            Command::IndexStatus(Default::default()),
            Command::DraftImage(crate::protocol::DraftImageArgs {
                instance_id: "instance".into(),
                draft_id: "draft".into(),
                action: crate::protocol::DraftImageAction::Begin { encoded_bytes: 10 },
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
                        owner_version: None,
                        protocol_version: PROTOCOL_VERSION,
                        capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                            .iter()
                            .map(|s| (*s).to_owned())
                            .collect(),
                        instance_id: "instance".into(),
                        operation_epoch: "instance.0".into(),
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
            } else if name == "draft.image" {
                assert_eq!(details["read_only"], false);
                assert_eq!(details["request_may_have_been_applied"], true);
                assert!(details.get("next_operation").is_none());
                assert!(hint.contains("begin a replacement for the same draft"));
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoint.json");
        let mut saved = endpoint();
        saved.protocol_version = PROTOCOL_VERSION - 1;
        let version = "1.2.3 0123456789abcdef0123456789abcdef01234567";
        // The fixture has no valid transport or address: no Hello or command can be sent.
        for (advertised, expected) in [
            (Some(version), Some(version)),
            (None, None),
            (Some("invalid"), None),
        ] {
            saved.owner_version = advertised.map(str::to_owned);
            let wire = serde_json::to_value(&saved).unwrap();
            assert_eq!(
                wire.get("owner_version").and_then(Value::as_str),
                advertised
            );
            assert_eq!(wire.get("owner_version").is_some(), advertised.is_some());
            std::fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
            let error = call(dir.path(), Command::Status(Default::default()), None, None)
                .await
                .unwrap_err();
            assert_version_mismatch(&error, PROTOCOL_VERSION - 1);
            let details = error.details.as_ref().unwrap();
            assert_eq!(
                details.get("owner_version").and_then(Value::as_str),
                expected
            );
            assert_eq!(details.get("owner_version").is_some(), expected.is_some());
        }
    }

    #[tokio::test]
    async fn changed_owner_instance_is_distinct_from_protocol_mismatch_before_dispatch() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "replacement-instance".into(),
                    operation_epoch: "replacement-instance.0".into(),
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
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                    operation_epoch: "instance.0".into(),
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
                max_model_requests: None,
                resume_recovery: false,
                human: false,
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
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: vec!["future.capability".into()],
                    instance_id: "instance".into(),
                    operation_epoch: "instance.0".into(),
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
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION - 1,
                    capabilities: Vec::new(),
                    instance_id: "instance".into(),
                    operation_epoch: "instance.0".into(),
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
                root: None,
                include_descendant_count: false,
                include_activity: false,
                include_detail: false,
                thread_ids: None,
                state: None,
                order: Default::default(),
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
                crate::control::OperationEpoch::new("instance"),
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
            crate::control::OperationEpoch::new("instance"),
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
            crate::control::OperationEpoch::new("instance"),
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
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities: crate::protocol::PROTOCOL_CAPABILITIES
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    instance_id: "instance".into(),
                    operation_epoch: "instance.0".into(),
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
            max_model_requests: None,
            resume_recovery: false,
            human: true,
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
                let task = tokio::spawn(serve_connection(server_stream, crate::control::OperationEpoch::new("instance"), requests, stop, work));
                let frame = match case {
                    "decode" => serde_json::json!({"protocol_version":PROTOCOL_VERSION,"operation_id":"instance:test","command":"no.such.command","args":{}}),
                    "validation" => serde_json::to_value(Request::new("instance:test", Command::ThreadList(crate::protocol::ThreadListArgs { limit: 0, ..Default::default() }))).unwrap(),
                    _ => status_frame(),
                };
                send_test_frame(&mut client, &frame).await;
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
                crate::control::OperationEpoch::new("instance"),
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

    #[tokio::test]
    async fn complete_shutdown_request_is_dispatched_even_when_peer_eof_is_ready() {
        use tokio::io::AsyncWriteExt;
        crate::test_watchdog::watchdog("IPC shutdown dispatch with buffered EOF", async {
            let (mut client, stream) = tokio::io::duplex(4096);
            let (requests, mut incoming) = mpsc::channel(1);
            write_frame(
                &mut client,
                &Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            )
            .await
            .unwrap();
            write_frame(
                &mut client,
                &Request::new(
                    "instance:shutdown",
                    Command::OwnerShutdown(crate::protocol::OwnerShutdownArgs {
                        instance_id: "instance".into(),
                    }),
                ),
            )
            .await
            .unwrap();
            client.shutdown().await.unwrap();
            // Both the request and EOF are ready before the owner polls the transport.
            let task = tokio::spawn(serve_connection(
                stream,
                crate::control::OperationEpoch::new("instance"),
                requests,
                CancellationToken::new(),
                server::WorkAdmission::new(),
            ));
            read_frame::<_, Welcome>(&mut client)
                .await
                .unwrap()
                .unwrap();
            let invocation = incoming
                .recv()
                .await
                .expect("complete shutdown request must reach Control");
            assert!(matches!(
                invocation.request.command,
                Command::OwnerShutdown(_)
            ));
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
                    crate::control::OperationEpoch::new("instance"),
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
    async fn explicit_stop_releases_connection_with_blocked_final_write() {
        crate::test_watchdog::watchdog("IPC stop during final write", async {
            let (mut client, stream) = tokio::io::duplex(4096);
            let (requests, mut incoming) = mpsc::channel(1);
            let stop = CancellationToken::new();
            let task = tokio::spawn(serve_connection(
                stream,
                crate::control::OperationEpoch::new("instance"),
                requests,
                stop.clone(),
                server::WorkAdmission::new(),
            ));
            send_test_frame(&mut client, &status_frame()).await;
            let invocation = incoming.recv().await.unwrap();
            invocation
                .reply
                .send(Response::success(
                    invocation.request.operation_id,
                    Value::String("x".repeat(8192)),
                ))
                .unwrap();

            // Observe that delivery started, but leave more bytes unread than
            // the duplex buffer can hold so delivery cannot finish.
            let frame_len = client.read_u32().await.unwrap() as usize;
            assert!(frame_len > 4096);
            stop.cancel();
            task.await.unwrap().unwrap();
            let mut tail = Vec::new();
            client.read_to_end(&mut tail).await.unwrap();
            assert!(tail.len() < frame_len);
        })
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn reply_delivery_releases_stalled_connections_at_deadline() {
        for read_reply in [false, true] {
            let (mut client, stream) = tokio::io::duplex(4096);
            let (requests, mut incoming) = mpsc::channel(1);
            let task = tokio::spawn(serve_connection(
                stream,
                crate::control::OperationEpoch::new("instance"),
                requests,
                CancellationToken::new(),
                server::WorkAdmission::new(),
            ));
            send_test_frame(&mut client, &status_frame()).await;
            let invocation = incoming.recv().await.unwrap();
            invocation
                .reply
                .send(Response::success(
                    invocation.request.operation_id,
                    Value::String("x".repeat(8192)),
                ))
                .unwrap();
            if read_reply {
                assert!(
                    read_frame::<_, Response<Value>>(&mut client)
                        .await
                        .unwrap()
                        .unwrap()
                        .ok
                );
            } else {
                assert!(client.read_u32().await.unwrap() > 4096);
            }
            let started = tokio::time::Instant::now();
            // Keep the peer alive while awaiting owner-side retirement.
            tokio::select! {
                result = task => result.unwrap().unwrap(),
                _ = tokio::time::sleep(FRAME_READ_TIMEOUT + std::time::Duration::from_secs(1)) => {
                    panic!("connection did not retire at the delivery deadline");
                }
            }
            assert_eq!(started.elapsed(), FRAME_READ_TIMEOUT);
            drop(client);
        }
    }

    #[tokio::test]
    async fn final_frame_keeps_connection_until_peer_closes() {
        crate::test_watchdog::watchdog("IPC final frame lifetime", async {
            for (reject, cutoff_before_delivery) in [(false, false), (true, false), (false, true)] {
                let (mut client, stream) = tokio::io::duplex(4096);
                let (requests, mut incoming) = mpsc::channel(1);
                let stop = CancellationToken::new();
                let connection = serve_connection(stream, crate::control::OperationEpoch::new("instance"), requests, stop.clone(), server::WorkAdmission::new());
                tokio::pin!(connection);
                let peer = async {
                    let mut frame = status_frame();
                    if reject { frame["command"] = Value::String("no.such.command".into()); }
                    send_test_frame(&mut client, &frame).await;
                    if !reject {
                        let invocation = incoming.recv().await.unwrap();
                        if cutoff_before_delivery { stop.cancel(); }
                        invocation.reply.send(Response::success(invocation.request.operation_id, Value::Bool(true))).unwrap();
                    }
                    let response = read_frame::<_, Response<Value>>(&mut client).await.unwrap().unwrap();
                    assert_eq!(response.ok, !reject);
                    stop.cancel();
                };
                tokio::select! {
                    () = peer => {},
                    result = &mut connection => panic!("connection ended before peer close: {result:?}"),
                }
                assert!(futures_util::poll!(&mut connection).is_pending(), "completed delivery must wait for EOF even after cutoff");
                drop(client);
                connection.await.unwrap();
            }
        }).await.unwrap();
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
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: "instance".into(),
                    operation_epoch: "instance.0".into(),
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
            None,
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
                    crate::control::OperationEpoch::new("instance"),
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
