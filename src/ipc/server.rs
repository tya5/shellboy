//! Lifetime of the local listener and its bounded connection tasks.
use super::{Invocation, serve_connection};
use crate::{
    app::DataOwner,
    protocol::{ApiError, Command, ErrorCode},
};
use std::{
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

mod endpoint;
use endpoint::EndpointFiles;

pub(crate) const MAX_WAIT_WORK: usize = 32;
const CONNECTION_HEADROOM: usize = 8;
const MAX_CONNECTIONS: usize = MAX_WAIT_WORK
    + MAX_IPC_WORK
    + MAX_CONTROL_WORK
    + MAX_OBSERVATION_WORK
    + MAX_MEMORY_OBSERVATION_WORK
    + CONNECTION_HEADROOM;
const MAX_IPC_WORK: usize = 24;
const MAX_CONTROL_WORK: usize = 2;
const MAX_OBSERVATION_WORK: usize = 2;
const MAX_MEMORY_OBSERVATION_WORK: usize = 1;

#[derive(Default)]
pub(crate) struct AdmissionRefusals {
    waits: AtomicU64,
    ordinary: AtomicU64,
    stop: AtomicU64,
    status: AtomicU64,
    memory: AtomicU64,
}
impl AdmissionRefusals {
    pub(crate) fn snapshot(&self) -> crate::status::IpcAdmissionStatus {
        use crate::status::{IpcAdmissionClass, IpcAdmissionStatus};
        let class = |limit, count: &AtomicU64| IpcAdmissionClass {
            limit,
            refused: count.load(Ordering::Relaxed),
        };
        IpcAdmissionStatus {
            max_connections: MAX_CONNECTIONS,
            wait: class(MAX_WAIT_WORK, &self.waits),
            ordinary: class(MAX_IPC_WORK, &self.ordinary),
            stop: class(MAX_CONTROL_WORK, &self.stop),
            status: class(MAX_OBSERVATION_WORK, &self.status),
            memory: class(MAX_MEMORY_OBSERVATION_WORK, &self.memory),
        }
    }
}

/// Transport budgets; accepted effects retain their permit after disconnection.
#[derive(Clone)]
pub(super) struct WorkAdmission {
    refused: Arc<AdmissionRefusals>,
    waits: Arc<Semaphore>,
    ordinary: Arc<Semaphore>,
    control: Arc<Semaphore>,
    observation: Arc<Semaphore>,
    memory_observation: Arc<Semaphore>,
}
pub(crate) fn is_wait(command: &Command) -> bool {
    matches!(command, Command::Wait(_) | Command::EventsWait(_))
}

impl WorkAdmission {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::with_refusals(Arc::default())
    }
    fn with_refusals(refused: Arc<AdmissionRefusals>) -> Self {
        Self {
            refused,
            waits: Arc::new(Semaphore::new(MAX_WAIT_WORK)),
            ordinary: Arc::new(Semaphore::new(MAX_IPC_WORK)),
            control: Arc::new(Semaphore::new(MAX_CONTROL_WORK)),
            observation: Arc::new(Semaphore::new(MAX_OBSERVATION_WORK)),
            memory_observation: Arc::new(Semaphore::new(MAX_MEMORY_OBSERVATION_WORK)),
        }
    }

    pub(super) fn try_acquire(&self, command: &Command) -> Result<OwnedSemaphorePermit, ApiError> {
        let (slots, refused) = match command {
            Command::RunStop(_)
            | Command::ShellStop(_)
            | Command::TurnInterrupt(_)
            | Command::ShellBackground(_)
            | Command::OwnerShutdown(_) => (&self.control, &self.refused.stop),
            Command::Status(args) if args.memory => {
                (&self.memory_observation, &self.refused.memory)
            }
            Command::Status(_) => (&self.observation, &self.refused.status),
            command if is_wait(command) => (&self.waits, &self.refused.waits),
            _ => (&self.ordinary, &self.refused.ordinary),
        };
        slots.clone().try_acquire_owned().map_err(|_| {
            let _ = refused.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1));
            ApiError::new(ErrorCode::InvalidState,"Local IPC work capacity is busy; this request was not accepted")
                .hint("Wait for pending work to complete, then retry with the same operation ID. Use shellboy status to observe work or stop the intended Run")
                .details(serde_json::json!({"phase":"admission","request_may_have_been_applied":false}))
        })
    }
}
pub(super) const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);
pub(super) const MAX_ACCEPT_BACKOFF: Duration = Duration::from_secs(1);

trait Acceptor: Send + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    fn accept(&mut self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

#[cfg(unix)]
impl Acceptor for tokio::net::UnixListener {
    type Stream = tokio::net::UnixStream;
    async fn accept(&mut self) -> io::Result<Self::Stream> {
        loop {
            let (stream, _) = tokio::net::UnixListener::accept(self).await?;
            if super::peer::verify(&stream).is_ok() {
                return Ok(stream);
            }
            // Unverified peers never receive a hello/welcome or occupy a
            // protocol task. Reject this connection, keeping the listener live.
            drop(stream);
            // A ready backlog of rejected peers must still allow shutdown.
            tokio::task::yield_now().await;
        }
    }
}

#[cfg(windows)]
struct PipeAcceptor {
    pipe: tokio::net::windows::named_pipe::NamedPipeServer,
    address: String,
    connected: bool,
}
#[cfg(windows)]
impl Acceptor for PipeAcceptor {
    type Stream = tokio::net::windows::named_pipe::NamedPipeServer;
    async fn accept(&mut self) -> io::Result<Self::Stream> {
        use tokio::net::windows::named_pipe::ServerOptions;
        if !self.connected {
            if let Err(error) = self.pipe.connect().await {
                use windows_sys::Win32::Foundation::{
                    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
                };
                if error.raw_os_error().is_some_and(|code| {
                    matches!(
                        code as u32,
                        ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
                    )
                }) {
                    if let Err(reset) = self.pipe.disconnect()
                        && reset.raw_os_error() != Some(ERROR_PIPE_NOT_CONNECTED as i32)
                    {
                        return Err(reset);
                    }
                    return Err(io::ErrorKind::ConnectionAborted.into());
                }
                return Err(error);
            }
            self.connected = true;
        }
        // If creation runs out of resources, retain the connected client while
        // the shared loop backs off; do not reconnect or silently discard it.
        let next = ServerOptions::new().create(&self.address)?;
        self.connected = false;
        Ok(std::mem::replace(&mut self.pipe, next))
    }
}

fn server_error(context: &str, error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::IoFailed, format!("Local IPC {context}: {error}"))
        .hint("Restart the owner to restore CLI access and inspect known operation receipts before repeating mutations")
}

/// Startup has no accepted IPC work to recover. Keep its advice separate from
/// a listener that failed after accepting requests; never suggest replay here.
fn startup_error(reason: &str, error: impl std::fmt::Display) -> ApiError {
    let (message, hint) = match reason {
        "existing_socket_live" => (
            "Startup was aborted because the existing endpoint is responding.",
            "Shut down the existing Shellboy instance normally, then start it again.",
        ),
        "existing_socket_probe_timeout" | "existing_socket_probe_failed" => (
            "Startup was aborted because the existing endpoint could not be confirmed inactive.",
            "Check the existing Shellboy instance's state. The endpoint was left in place for safety.",
        ),
        "namespace_lease_failed" => (
            "Startup was aborted because exclusive access to the temporary IPC namespace could not be acquired.",
            "Check permissions and lock availability in the OS temporary directory, then start Shellboy again.",
        ),
        _ => (
            "The local IPC listener could not be started.",
            "Check free space and permissions in the OS temporary IPC namespace and communication resource usage, then start Shellboy again.",
        ),
    };
    ApiError::new(ErrorCode::IoFailed, message)
        .hint(hint)
        .details(serde_json::json!({"component":"local_ipc", "phase":"startup", "reason":reason, "cause":error.to_string()}))
}

fn startup_context(error: ApiError) -> ApiError {
    // Specific namespace/startup diagnostics already carry a reason and advice.
    if error
        .details
        .as_ref()
        .is_some_and(|details| details["component"] == "local_ipc" && details["reason"].is_string())
    {
        error
    } else {
        startup_error("endpoint_preparation_failed", error)
    }
}

fn retryable(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::TimedOut
            | io::ErrorKind::OutOfMemory
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    ) {
        return true;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            ERROR_NO_SYSTEM_RESOURCES, ERROR_NOT_ENOUGH_MEMORY, ERROR_OUTOFMEMORY, ERROR_PIPE_BUSY,
            ERROR_TOO_MANY_OPEN_FILES,
        };
        if error.raw_os_error().is_some_and(|code| {
            matches!(
                code as u32,
                ERROR_TOO_MANY_OPEN_FILES
                    | ERROR_NOT_ENOUGH_MEMORY
                    | ERROR_OUTOFMEMORY
                    | ERROR_NO_SYSTEM_RESOURCES
                    | ERROR_PIPE_BUSY
            )
        }) {
            return true;
        }
    }
    false
}

async fn wait_backoff(stop: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        biased;
        _ = stop.cancelled() => false,
        _ = tokio::time::sleep(delay) => true,
    }
}

#[derive(Clone, Default)]
struct TransportStop {
    accepting: CancellationToken,
    listener_closed: CancellationToken,
    delivery: CancellationToken,
}

async fn accept_connections<A: Acceptor>(
    mut listener: A,
    instance: String,
    requests: mpsc::Sender<Invocation>,
    stop: TransportStop,
    files: EndpointFiles,
    failure: watch::Sender<Option<ApiError>>,
    work: WorkAdmission,
) -> Result<(), ApiError> {
    let listener_closed = stop.listener_closed.clone().drop_guard();
    let mut clients = JoinSet::new();
    let mut backoff = ACCEPT_BACKOFF;
    let result = loop {
        tokio::select! {
            biased;
            _ = stop.accepting.cancelled() => break Ok(()),
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < MAX_CONNECTIONS => {
                match accepted {
                    Ok(stream) => {
                        backoff = ACCEPT_BACKOFF;
                        clients.spawn(serve_connection(stream, instance.clone(), requests.clone(), stop.delivery.clone(), work.clone()));
                    }
                    Err(error) if retryable(&error) => {
                        if !wait_backoff(&stop.accepting, backoff).await {
                            break Ok(());
                        }
                        backoff = (backoff * 2).min(MAX_ACCEPT_BACKOFF);
                    }
                    Err(error) => break Err(server_error("listener stopped", error)),
                }
            },
        }
    };
    drop(listener);
    let result = result.and(files.cleanup().await);
    // Report a fatal listener failure without stopping accepted connections.
    if let Err(error) = &result {
        failure.send_replace(Some(error.clone()));
    }
    drop(listener_closed);
    while clients.join_next().await.is_some() {}
    result
}

pub struct LocalServer {
    stop: TransportStop,
    task: JoinHandle<Result<(), ApiError>>,
    failure: watch::Receiver<Option<ApiError>>,
}
impl LocalServer {
    pub async fn bind(
        owner: &DataOwner,
        requests: mpsc::Sender<Invocation>,
    ) -> Result<Self, ApiError> {
        let instance = owner.instance_id.clone();
        #[cfg(unix)]
        let (listener, _, socket) = endpoint::bind_unix(owner, instance.clone())
            .await
            .map_err(startup_context)?;
        #[cfg(windows)]
        let (listener, endpoint) = {
            use crate::{app::Endpoint, protocol::PROTOCOL_VERSION};
            use tokio::net::windows::named_pipe::ServerOptions;
            let address = format!(r"\\.\pipe\shellboy-{instance}");
            let pipe = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&address)
                .map_err(|error| startup_error("bind_failed", error))?;
            (
                PipeAcceptor {
                    pipe,
                    address: address.clone(),
                    connected: false,
                },
                Endpoint {
                    owner_version: None,
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: instance.clone(),
                    transport: "pipe".into(),
                    address,
                },
            )
        };
        let files = EndpointFiles {
            metadata: owner.loaded.data_dir.join("endpoint.json"),
            instance: instance.clone(),
            #[cfg(unix)]
            socket,
        };
        #[cfg(windows)]
        if let Err(error) = owner.publish_endpoint(endpoint).await {
            drop(listener);
            let _ = files.cleanup().await;
            return Err(startup_context(error));
        }
        Ok(Self::start(
            listener,
            instance,
            requests,
            files,
            owner.ipc_refusals.clone(),
        ))
    }

    fn start<A: Acceptor>(
        listener: A,
        instance: String,
        requests: mpsc::Sender<Invocation>,
        files: EndpointFiles,
        refused: Arc<AdmissionRefusals>,
    ) -> Self {
        let stop = TransportStop::default();
        let token = stop.clone();
        let (failure_send, failure) = watch::channel(None);
        let task = tokio::spawn(accept_connections(
            listener,
            instance,
            requests,
            token,
            files,
            failure_send,
            WorkAdmission::with_refusals(refused),
        ));
        Self {
            stop,
            task,
            failure,
        }
    }

    /// One terminal transport failure; this does not change Run/Turn state.
    pub fn failures(&self) -> watch::Receiver<Option<ApiError>> {
        self.failure.clone()
    }

    pub(crate) fn stop_accepting(&self) -> impl std::future::Future<Output = ()> + use<> {
        let accepting = self.stop.accepting.clone();
        let listener_closed = self.stop.listener_closed.clone();
        async move {
            accepting.cancel();
            listener_closed.cancelled().await;
        }
    }

    pub async fn shutdown(self) -> Result<(), ApiError> {
        self.stop_accepting().await;
        self.stop.delivery.cancel();
        self.task
            .await
            .map_err(|error| server_error("task failed", error))?
    }
}

#[cfg(test)]
pub(super) mod tests;
