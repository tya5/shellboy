//! Lifetime of the local listener and its bounded connection tasks.
use super::{Invocation, serve_connection};
use crate::{
    app::DataOwner,
    protocol::{ApiError, Command, ErrorCode},
};
use std::{future::Future, io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

mod endpoint;
use endpoint::EndpointFiles;

const MAX_CONNECTIONS: usize = 32;
const MAX_IPC_WORK: usize = 24;
const MAX_CONTROL_WORK: usize = 2;
const MAX_OBSERVATION_WORK: usize = 2;

/// Transport budgets; accepted effects retain their permit after disconnection.
#[derive(Clone)]
pub(super) struct WorkAdmission {
    ordinary: Arc<Semaphore>,
    control: Arc<Semaphore>,
    observation: Arc<Semaphore>,
}
impl WorkAdmission {
    pub(super) fn new() -> Self {
        Self {
            ordinary: Arc::new(Semaphore::new(MAX_IPC_WORK)),
            control: Arc::new(Semaphore::new(MAX_CONTROL_WORK)),
            observation: Arc::new(Semaphore::new(MAX_OBSERVATION_WORK)),
        }
    }
    pub(super) fn try_acquire(&self, command: &Command) -> Result<OwnedSemaphorePermit, ApiError> {
        let slots = match command {
            Command::RunStop(_) | Command::ShellStop(_) => &self.control,
            Command::Status(_) => &self.observation,
            _ => &self.ordinary,
        };
        slots.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(ErrorCode::InvalidState,"Local IPC work capacity is busy; this request was not accepted")
                .hint("Wait for pending work to complete, then retry with the same operation ID. Use shellboy status to observe work or stop the intended Run")
                .details(serde_json::json!({"phase":"admission","request_may_have_been_applied":false}))
        })
    }
}
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);
const MAX_ACCEPT_BACKOFF: Duration = Duration::from_secs(1);

trait Acceptor: Send + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    fn accept(&mut self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

#[cfg(unix)]
impl Acceptor for tokio::net::UnixListener {
    type Stream = tokio::net::UnixStream;
    async fn accept(&mut self) -> io::Result<Self::Stream> {
        tokio::net::UnixListener::accept(self)
            .await
            .map(|(stream, _)| stream)
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

async fn accept_connections<A: Acceptor>(
    mut listener: A,
    instance: String,
    requests: mpsc::Sender<Invocation>,
    stop: CancellationToken,
) -> Result<(), ApiError> {
    let mut clients = JoinSet::new();
    let work = WorkAdmission::new();
    let mut backoff = ACCEPT_BACKOFF;
    let result = loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break Ok(()),
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < MAX_CONNECTIONS => {
                match accepted {
                    Ok(stream) => {
                        backoff = ACCEPT_BACKOFF;
                        clients.spawn(serve_connection(stream, instance.clone(), requests.clone(), stop.clone(), work.clone()));
                    }
                    Err(error) if retryable(&error) => {
                        if !wait_backoff(&stop, backoff).await {
                            break Ok(());
                        }
                        backoff = (backoff * 2).min(MAX_ACCEPT_BACKOFF);
                    }
                    Err(error) => break Err(server_error("listener stopped", error)),
                }
            },
        }
    };
    // Release the listener before disconnecting observers. Invocation processing
    // belongs to Control and continues independently once it has been accepted.
    drop(listener);
    stop.cancel();
    while clients.join_next().await.is_some() {}
    result
}

pub struct LocalServer {
    stop: CancellationToken,
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
        let (listener, endpoint, socket) =
            endpoint::bind_unix(owner.loaded.data_dir.clone(), instance.clone()).await?;
        #[cfg(windows)]
        let (listener, endpoint) = {
            use crate::{app::Endpoint, protocol::PROTOCOL_VERSION};
            use tokio::net::windows::named_pipe::ServerOptions;
            let address = format!(r"\\.\pipe\shellboy-{instance}");
            let pipe = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&address)
                .map_err(|error| server_error("bind failed", error))?;
            (
                PipeAcceptor {
                    pipe,
                    address: address.clone(),
                    connected: false,
                },
                Endpoint {
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
        if let Err(error) = owner.publish_endpoint(endpoint).await {
            drop(listener);
            let _ = files.cleanup().await;
            return Err(error);
        }
        Ok(Self::start(listener, instance, requests, files))
    }

    fn start<A: Acceptor>(
        listener: A,
        instance: String,
        requests: mpsc::Sender<Invocation>,
        files: EndpointFiles,
    ) -> Self {
        let stop = CancellationToken::new();
        let token = stop.clone();
        let (failure_send, failure) = watch::channel(None);
        let task = tokio::spawn(async move {
            let result = accept_connections(listener, instance, requests, token).await;
            let result = result.and(files.cleanup().await);
            if let Err(error) = &result {
                failure_send.send_replace(Some(error.clone()));
            }
            result
        });
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

    pub async fn shutdown(self) -> Result<(), ApiError> {
        self.stop.cancel();
        self.task
            .await
            .map_err(|error| server_error("task failed", error))?
    }
}

#[cfg(test)]
mod tests;
