//! Pipe-based shell executions. Callers commit execution intent before `spawn` and
//! persist all output events before publishing the completion notification.
#[cfg(unix)]
use crate::config::ShellKind;
use crate::{
    config::Config,
    protocol::{ApiError, ErrorCode, ShellState, ShellToolArgs},
};
#[cfg(unix)]
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, watch},
    time::{Duration, Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use std::sync::Mutex as StdMutex;
use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(unix)]
use tokio::process::Command;
#[cfg(unix)]
mod unix_group;
#[cfg(unix)]
use unix_group::{cleanup_group, signal_group};

mod io_session;
pub(crate) mod timing;
use io_session::IoSession;

const READ_CHUNK: usize = 8192;
enum ShellStdin {
    #[cfg(unix)]
    Unix(tokio::process::ChildStdin),
    #[cfg(windows)]
    Windows(windows_stdin::NativeStdin),
}
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
#[cfg(windows)]
mod windows;
#[doc(hidden)]
pub mod windows_command;
#[cfg(windows)]
mod windows_stdin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Its private semaphore permit accounts for buffered bytes until the caller
/// has committed the chunk and drops it. Do not clone bytes into an unbounded queue.
#[derive(Debug)]
pub struct OutputChunk {
    pub sequence: u64,
    pub stream: OutputStream,
    pub bytes: Vec<u8>,
    _budget: OwnedSemaphorePermit,
}
#[derive(Debug)]
pub enum ShellEvent {
    Output(OutputChunk),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellCompletion {
    pub execution_id: String,
    pub state: ShellState,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub reason: Option<String>,
    pub forced: bool,
    pub output_complete: bool,
    pub group_cleanup_confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StdinWriteStatus {
    Complete,
    Timeout,
    Closed,
    Stopped,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdinWriteResult {
    pub bytes_written: u64,
    pub status: StdinWriteStatus,
    pub stdin_open: bool,
    pub reason: Option<String>,
}

struct HandleInner {
    stdin: Arc<Mutex<Option<ShellStdin>>>,
    stop: CancellationToken,
    finished: CancellationToken,
    stdin_limit: u64,
    write_timeout: Duration,
}
impl Drop for HandleInner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
#[derive(Clone)]
pub struct ShellHandle {
    inner: Arc<HandleInner>,
}
impl std::fmt::Debug for ShellHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellHandle").finish_non_exhaustive()
    }
}
impl ShellHandle {
    /// Nonblocking and independent of the stdin writer and output backpressure.
    pub fn stop(&self) {
        self.inner.stop.cancel();
    }
    pub fn is_finished(&self) -> bool {
        self.inner.finished.is_cancelled()
    }
    pub async fn write(&self, text: &str, encoding: &str) -> Result<StdinWriteResult, ApiError> {
        let bytes = encode_input(text, encoding, self.inner.stdin_limit)?;
        let deadline = Instant::now() + self.inner.write_timeout;
        let mut stdin = tokio::select! {
            biased;
            _ = self.inner.stop.cancelled() => return Ok(write_result(0,StdinWriteStatus::Stopped,false,None)),
            _ = self.inner.finished.cancelled() => return Ok(write_result(0,StdinWriteStatus::Closed,false,None)),
            result = timeout_at(deadline,self.inner.stdin.lock()) => match result { Ok(guard) => guard, Err(_) => return Ok(write_result(0,StdinWriteStatus::Timeout,true,Some("stdin queue deadline exceeded".into()))) }
        };
        Ok(write_bytes(
            &mut stdin,
            &bytes,
            deadline,
            &self.inner.stop,
            &self.inner.finished,
        )
        .await)
    }
    /// EOF closes the pipe once, in the same serial order as writes.
    pub async fn eof(&self) -> Result<(), ApiError> {
        let mut stdin = timeout(self.inner.write_timeout, self.inner.stdin.lock())
            .await
            .map_err(|_| ApiError::new(ErrorCode::IoFailed, "stdin EOF queue deadline exceeded"))?;
        stdin.take();
        Ok(())
    }
}
pub struct ShellExecution {
    pub handle: ShellHandle,
    pub events: mpsc::Receiver<ShellEvent>,
    pub completion: watch::Receiver<Option<ShellCompletion>>,
}

#[derive(Clone)]
pub struct ShellSupervisor {
    config: Config,
    // One-way owner-lifecycle fact, shared with the existing Quit mailbox.
    starts_closed: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(unix)]
    native_groups: Arc<StdMutex<std::collections::BTreeSet<i32>>>,
    #[cfg(unix)]
    force_requested: Arc<std::sync::atomic::AtomicBool>,
}
impl ShellSupervisor {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            starts_closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(unix)]
            native_groups: Arc::new(StdMutex::new(std::collections::BTreeSet::new())),
            #[cfg(unix)]
            force_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    pub(crate) fn start_gate(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.starts_closed.clone()
    }
    pub(crate) fn close_start_admission(&self) {
        self.starts_closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    /// Final best effort before process::exit; only this live supervisor's
    /// groups are eligible, never identifiers restored from saved metadata.
    pub fn force_kill_live(&self) {
        self.close_start_admission();
        #[cfg(unix)]
        {
            self.force_requested
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // A native spawn may own this lock. Forced exit must never wait
            // behind it on either the UI or the independent deadline observer.
            let groups = match self.native_groups.try_lock() {
                Ok(groups) => Some(groups),
                Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            if let Some(groups) = groups {
                for group in groups.iter() {
                    let _ = signal_group(*group, libc::SIGKILL);
                }
            }
        }
        // Windows kernel Job handles already use kill-on-close; process exit
        // closes those owned handles even though Rust destructors do not run.
    }
    pub(crate) fn starts_closed(&self) -> bool {
        self.starts_closed.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn admit_start(&self) -> Result<(), ApiError> {
        // This load is the start-admission linearization point relative to the
        // Quit store. No await may occur between admission and native creation.
        if self.starts_closed() {
            Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner shutdown cancelled this shell before process creation",
            )
            .details(serde_json::json!({"reason":"shutdown_before_spawn"})))
        } else {
            Ok(())
        }
    }
    /// This method never commits DB state. A successful call means OS process
    /// creation, not durable acceptance. The Control lane owns that boundary.
    pub async fn spawn(
        &self,
        execution_id: String,
        cwd: PathBuf,
        args: ShellToolArgs,
        env: BTreeMap<String, String>,
    ) -> Result<ShellExecution, ApiError> {
        self.spawn_inner(execution_id, cwd, args, env, None).await
    }
    pub(crate) async fn spawn_guarded(
        &self,
        execution_id: String,
        cwd: PathBuf,
        args: ShellToolArgs,
        env: BTreeMap<String, String>,
        gate: Arc<tokio::sync::Mutex<bool>>,
    ) -> Result<ShellExecution, ApiError> {
        self.spawn_inner(execution_id, cwd, args, env, Some(gate))
            .await
    }
    async fn spawn_inner(
        &self,
        execution_id: String,
        cwd: PathBuf,
        args: ShellToolArgs,
        env: BTreeMap<String, String>,
        gate: Option<Arc<tokio::sync::Mutex<bool>>>,
    ) -> Result<ShellExecution, ApiError> {
        args.validate()?;
        if execution_id.trim().is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "execution_id must not be empty",
            ));
        }
        // Resolve both encodings before any process or external side effect.
        resolve_encoding(&args.input_encoding)?;
        resolve_encoding(&args.output_encoding)?;
        let initial = args
            .initial_stdin
            .as_deref()
            .map(|text| {
                encode_input(
                    text,
                    &args.input_encoding,
                    self.config.limits.shell_stdin_bytes,
                )
            })
            .transpose()?;
        let budget_size = usize::try_from(self.config.io.output_buffer_bytes)
            .ok()
            .filter(|n| *n > 0 && *n <= Semaphore::MAX_PERMITS)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::ConfigInvalid,
                    "output buffer budget is unsupported",
                )
            })?;
        #[cfg(windows)]
        {
            self.spawn_windows(execution_id, cwd, args, env, initial, budget_size, gate)
                .await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (cwd, env, initial, budget_size);
            Err(ApiError::new(
                ErrorCode::InvalidState,
                "shell executions are unsupported on this platform",
            ))
        }
        #[cfg(unix)]
        {
            self.spawn_unix(execution_id, cwd, args, env, initial, budget_size, gate)
                .await
        }
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    async fn spawn_unix(
        &self,
        execution_id: String,
        cwd: PathBuf,
        args: ShellToolArgs,
        env: BTreeMap<String, String>,
        initial: Option<Vec<u8>>,
        budget_size: usize,
        gate: Option<Arc<tokio::sync::Mutex<bool>>>,
    ) -> Result<ShellExecution, ApiError> {
        use std::process::Stdio;
        let mut command = launch_command(&self.config, &args.command);
        command
            .current_dir(cwd)
            .envs(&self.config.shell.env)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let turn_gate = match &gate {
            Some(gate) => Some(gate.lock().await),
            None => None,
        };
        if turn_gate.as_ref().is_some_and(|guard| **guard) {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Turn interrupted before shell spawn",
            )
            .details(serde_json::json!({"reason":"turn_interrupted_before_spawn"})));
        }
        let spawned_at = Instant::now();
        // Serialize native publication with emergency force cleanup. No await
        // or DB/file work occurs while this short native ownership lock is held.
        let mut groups = self.native_groups.lock().unwrap_or_else(|e| e.into_inner());
        self.admit_start()?;
        let mut child = command.spawn().map_err(|error| {
            ApiError::new(ErrorCode::IoFailed, format!("shell spawn failed: {error}"))
        })?;
        let pgid = child.id().ok_or_else(|| {
            ApiError::new(ErrorCode::IoFailed, "spawned shell has no process handle")
        })? as i32;
        groups.insert(pgid);
        if self
            .force_requested
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let _ = signal_group(pgid, libc::SIGKILL);
        }
        drop(groups);
        drop(turn_gate);
        let owned_group = unix_group::OwnedGroup::new(self.native_groups.clone(), pgid);
        let stdin = Arc::new(Mutex::new(child.stdin.take().map(ShellStdin::Unix)));
        let stdout = child.stdout.take().expect("stdout configured as piped");
        let stderr = child.stderr.take().expect("stderr configured as piped");
        let (io, handle, rx) = IoSession::start(
            stdin,
            stdout,
            stderr,
            initial,
            args.stdin_open,
            budget_size,
            &self.config,
        );
        let (done_tx, done_rx) = watch::channel(None);
        let term_grace = Duration::from_millis(self.config.io.term_grace_ms);
        let drain_timeout = Duration::from_millis(self.config.io.drain_timeout_ms);
        tokio::spawn(async move {
            let mut forced = false;
            let mut stopped_before_exit = false;
            let mut kill_at = None;
            let status = tokio::select! {
                biased;
                result = child.wait() => result,
                _ = io.stop.cancelled() => {
                    stopped_before_exit = true;
                    let _ = signal_group(pgid,libc::SIGTERM);
                    kill_at = Some(Instant::now()+term_grace);
                    match timeout(term_grace,child.wait()).await {
                        Ok(result) => result,
                        Err(_) => {
                            forced = true;
                            let _ = signal_group(pgid,libc::SIGKILL);
                            match timeout(drain_timeout,child.wait()).await {
                                Ok(result) => result,
                                Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut,"direct shell reap deadline exceeded")),
                            }
                        }
                    }
                }
            };
            if let Err(error) = &status {
                record_failure(
                    &io.failure,
                    FailureKind::ProcessWait,
                    format!("shell wait failed: {error}"),
                );
            }
            let (group_confirmed, group_forced) =
                cleanup_group(pgid, kill_at, term_grace, drain_timeout).await;
            owned_group.complete(group_confirmed);
            forced |= group_forced;
            let (state, reason, output_complete) =
                io.finish(drain_timeout, stopped_before_exit).await;
            let completion = ShellCompletion {
                execution_id,
                state,
                exit_code: status.as_ref().ok().and_then(|s| s.code()),
                exit_signal: {
                    use std::os::unix::process::ExitStatusExt;
                    status.ok().and_then(|s| s.signal())
                },
                reason,
                forced,
                output_complete,
                group_cleanup_confirmed: group_confirmed,
                elapsed_ms: Some(completion_elapsed_ms(spawned_at)),
            };
            let _ = done_tx.send(Some(completion));
        });
        Ok(ShellExecution {
            handle,
            events: rx,
            completion: done_rx,
        })
    }
}

// A stored duration is a terminal supervisor observation, independent of epoch
// time and of subsequent DB/consumer work. SQLite JSON integers are signed.
fn completion_elapsed_ms(spawned_at: Instant) -> u64 {
    spawned_at.elapsed().as_millis().min(i64::MAX as u128) as u64
}

// Shared by Unix Bash and the native Windows Git Bash argv adapter.
fn bash_arguments(command: &str) -> [&str; 5] {
    ["--noprofile", "--norc", "-c", "--", command]
}

#[cfg(unix)]
fn launch_command(config: &Config, command: &str) -> Command {
    match config.shell.kind {
        ShellKind::Bash => {
            let mut cmd = Command::new(
                config
                    .shell
                    .executable
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("bash")),
            );
            cmd.args(bash_arguments(command));
            cmd
        }
        ShellKind::Powershell => {
            let mut cmd = Command::new(config.shell.executable.clone().unwrap_or_else(|| {
                PathBuf::from(if cfg!(windows) {
                    "powershell.exe"
                } else {
                    "pwsh"
                })
            }));
            let utf16: Vec<u8> = command.encode_utf16().flat_map(u16::to_le_bytes).collect();
            cmd.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-OutputFormat",
                "Text",
                "-EncodedCommand",
                &STANDARD.encode(utf16),
            ]);
            cmd
        }
    }
}

struct Publisher {
    tx: mpsc::Sender<ShellEvent>,
    sequence: u64,
    total: u64,
    raw_limit: u64,
}
struct OutputSink {
    publisher: Mutex<Publisher>,
    budget: Arc<Semaphore>,
    chunk_size: usize,
    stop: CancellationToken,
    failure: Arc<StdMutex<ShellFailures>>,
}

async fn pump<R: AsyncRead + Unpin>(
    mut reader: R,
    stream: OutputStream,
    sink: Arc<OutputSink>,
) -> bool {
    loop {
        let mut bytes = vec![0; sink.chunk_size];
        let n = match reader.read(&mut bytes).await {
            Ok(0) => return true,
            Ok(n) => n,
            Err(error) => {
                record_failure(
                    &sink.failure,
                    FailureKind::OutputRead(stream),
                    format!("output read failed: {error}"),
                );
                sink.stop.cancel();
                return false;
            }
        };
        bytes.truncate(n);
        // Each pipe owns one fixed read buffer outside the queued-byte budget.
        // Reserving the whole budget before read could starve the other stream.
        let permit = match sink.budget.clone().acquire_many_owned(n as u32).await {
            Ok(p) => p,
            Err(_) => return false,
        };
        let mut publisher = sink.publisher.lock().await;
        let remaining = publisher.raw_limit.saturating_sub(publisher.total);
        if n as u64 > remaining {
            bytes.truncate(remaining as usize);
            record_failure(
                &sink.failure,
                FailureKind::RawLimit,
                "raw output limit exceeded; execution stopped and output truncated".into(),
            );
            sink.stop.cancel();
        }
        publisher.total += bytes.len() as u64;
        if bytes.is_empty() {
            continue;
        }
        let sequence = publisher.sequence;
        publisher.sequence += 1;
        let event = ShellEvent::Output(OutputChunk {
            sequence,
            stream,
            bytes,
            _budget: permit,
        });
        if publisher.tx.send(event).await.is_err() {
            record_failure(
                &sink.failure,
                FailureKind::OutputReceiver(stream),
                "output receiver disconnected".into(),
            );
            sink.stop.cancel();
            return false;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    InitialStdin,
    ProcessWait,
    #[cfg(windows)]
    Terminate,
    #[cfg(windows)]
    ActiveProcesses,
    #[cfg(windows)]
    GroupConfirm,
    OutputRead(OutputStream),
    OutputReceiver(OutputStream),
    RawLimit,
    Drain,
}

#[derive(Default)]
struct ShellFailures {
    // One reason per finite failure kind (including stream) bounds diagnostics
    // even when a stopped producer continues to exceed the raw-byte limit.
    reasons: Vec<(FailureKind, String)>,
    truncated: bool,
}
impl ShellFailures {
    fn completion_state(&self, stopped: bool) -> ShellState {
        if self
            .reasons
            .iter()
            .any(|(kind, _)| *kind != FailureKind::Drain)
        {
            ShellState::Failed
        } else if stopped {
            ShellState::Stopped
        } else {
            ShellState::Exited
        }
    }
    fn reason(&self) -> Option<String> {
        (!self.reasons.is_empty()).then(|| {
            self.reasons
                .iter()
                .map(|(_, reason)| reason.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
    fn output_complete(&self, drained: bool) -> bool {
        drained
            && !self.truncated
            && !self.reasons.iter().any(|(kind, _)| {
                matches!(
                    kind,
                    FailureKind::OutputRead(_)
                        | FailureKind::OutputReceiver(_)
                        | FailureKind::Drain
                )
            })
    }
}
fn record_failure(failure: &StdMutex<ShellFailures>, kind: FailureKind, reason: String) {
    let mut value = failure.lock().expect("failure mutex");
    value.truncated |= kind == FailureKind::RawLimit;
    if !value.reasons.iter().any(|(previous, _)| *previous == kind) {
        value.reasons.push((kind, reason));
    }
}
fn write_result(
    bytes_written: u64,
    status: StdinWriteStatus,
    stdin_open: bool,
    reason: Option<String>,
) -> StdinWriteResult {
    StdinWriteResult {
        bytes_written,
        status,
        stdin_open,
        reason,
    }
}
async fn write_bytes(
    stdin: &mut Option<ShellStdin>,
    bytes: &[u8],
    deadline: Instant,
    stop: &CancellationToken,
    finished: &CancellationToken,
) -> StdinWriteResult {
    #[cfg(windows)]
    {
        let Some(ShellStdin::Windows(pipe)) = stdin.as_mut() else {
            return write_result(0, StdinWriteStatus::Closed, false, None);
        };
        let result = pipe.write(bytes, deadline, stop, finished).await;
        if !result.stdin_open {
            stdin.take();
        }
        result
    }
    #[cfg(unix)]
    {
        let Some(ShellStdin::Unix(pipe)) = stdin.as_mut() else {
            return write_result(0, StdinWriteStatus::Closed, false, None);
        };
        let mut written = 0;
        while written < bytes.len() {
            let result = tokio::select! {
                biased;
                _=stop.cancelled()=>return write_result(written as u64,StdinWriteStatus::Stopped,false,None),
                _=finished.cancelled()=>return write_result(written as u64,StdinWriteStatus::Closed,false,None),
                result=timeout_at(deadline,pipe.write(&bytes[written..]))=>result,
            };
            match result {
                Err(_) => {
                    return write_result(
                        written as u64,
                        StdinWriteStatus::Timeout,
                        true,
                        Some("stdin write deadline exceeded; do not automatically resend".into()),
                    );
                }
                Ok(Err(error)) => {
                    stdin.take();
                    return write_result(
                        written as u64,
                        StdinWriteStatus::Failed,
                        false,
                        Some(format!("stdin write failed: {error}")),
                    );
                }
                Ok(Ok(0)) => {
                    stdin.take();
                    return write_result(written as u64, StdinWriteStatus::Closed, false, None);
                }
                Ok(Ok(n)) => written += n,
            }
        }
        write_result(written as u64, StdinWriteStatus::Complete, true, None)
    }
}

enum InputEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Legacy(&'static encoding_rs::Encoding),
}
fn resolve_encoding(label: &str) -> Result<InputEncoding, ApiError> {
    let normalized = label.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "utf8" | "utf-8" => Ok(InputEncoding::Utf8),
        "utf16le" | "utf-16le" => Ok(InputEncoding::Utf16Le),
        "utf16be" | "utf-16be" => Ok(InputEncoding::Utf16Be),
        "utf16" | "utf-16" => Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "UTF-16 requires explicit LE or BE byte order",
        )),
        "cp932" | "windows-31j" | "ms932" => Ok(InputEncoding::Legacy(encoding_rs::SHIFT_JIS)),
        _ => encoding_rs::Encoding::for_label(normalized.as_bytes())
            .map(|encoding| {
                if encoding == encoding_rs::UTF_16LE {
                    InputEncoding::Utf16Le
                } else if encoding == encoding_rs::UTF_16BE {
                    InputEncoding::Utf16Be
                } else if encoding == encoding_rs::UTF_8 {
                    InputEncoding::Utf8
                } else {
                    InputEncoding::Legacy(encoding)
                }
            })
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidArgument, "unknown text encoding")),
    }
}
/// Strictly encode the complete input before exposing any bytes to the process.
pub fn encode_input(text: &str, label: &str, limit: u64) -> Result<Vec<u8>, ApiError> {
    let bytes = match resolve_encoding(label)? {
        InputEncoding::Utf8 => text.as_bytes().to_vec(),
        InputEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        InputEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        InputEncoding::Legacy(encoding) => {
            let (bytes, _, errors) = encoding.encode(text);
            if errors {
                return Err(ApiError::new(
                    ErrorCode::InvalidArgument,
                    "stdin cannot be represented in the selected encoding",
                ));
            }
            bytes.into_owned()
        }
    };
    if bytes.len() as u64 > limit {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "encoded stdin exceeds the byte limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod output_failure_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn raw_limit_preserves_truncation_after_an_initial_stdin_failure() {
        let failure = Arc::new(StdMutex::new(ShellFailures::default()));
        record_failure(
            &failure,
            FailureKind::InitialStdin,
            "initial_stdin_Timeout: 0 bytes delivered".into(),
        );
        let (mut writer, reader) = tokio::io::duplex(32);
        writer.write_all(b"abcdefghijkl").await.unwrap();
        drop(writer);
        let (tx, mut events) = mpsc::channel(4);
        let stop = CancellationToken::new();
        let sink = Arc::new(OutputSink {
            publisher: Mutex::new(Publisher {
                tx,
                sequence: 0,
                total: 0,
                raw_limit: 4,
            }),
            budget: Arc::new(Semaphore::new(32)),
            chunk_size: 16,
            stop: stop.clone(),
            failure: failure.clone(),
        });
        let pump = tokio::spawn(pump(reader, OutputStream::Stdout, sink));
        let ShellEvent::Output(chunk) = events.recv().await.unwrap();
        assert_eq!(chunk.bytes, b"abcd");
        drop(chunk);
        assert!(events.recv().await.is_none());
        assert!(pump.await.unwrap());
        assert!(stop.is_cancelled());
        let failure = failure.lock().unwrap();
        assert!(failure.truncated);
        assert!(!failure.output_complete(true));
        let reason = failure.reason().unwrap();
        assert!(reason.contains("initial_stdin_Timeout"));
        assert!(
            reason.contains("raw output limit"),
            "truncation must not disappear behind the first failure: {reason}"
        );
    }
}
