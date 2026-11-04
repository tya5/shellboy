//! Ownership of the existing bounded pipe tasks, independent of native process cleanup.
use super::*;

pub(super) struct IoSession {
    pub(super) stop: CancellationToken,
    finished: CancellationToken,
    pub(super) failure: Arc<StdMutex<ShellFailures>>,
    stdout_task: tokio::task::JoinHandle<bool>,
    stderr_task: tokio::task::JoinHandle<bool>,
    initial_task: tokio::task::JoinHandle<()>,
    stdout_waiting: Arc<std::sync::atomic::AtomicBool>,
    stderr_waiting: Arc<std::sync::atomic::AtomicBool>,
    progress: Arc<std::sync::atomic::AtomicU64>,
}

async fn pump_until_stalled(
    task: &mut tokio::task::JoinHandle<bool>,
    waiting: &std::sync::atomic::AtomicBool,
    progress: &std::sync::atomic::AtomicU64,
    stall_timeout: Duration,
) -> bool {
    let mut blocked_since = None;
    let mut last_progress = progress.load(std::sync::atomic::Ordering::Relaxed);
    loop {
        tokio::select! {
            result = &mut *task => return result.is_ok_and(|complete| complete),
            _ = tokio::time::sleep(Duration::from_millis(5)) => {
                let current_progress = progress.load(std::sync::atomic::Ordering::Relaxed);
                if current_progress != last_progress {
                    last_progress = current_progress;
                    blocked_since = None;
                }
                if waiting.load(std::sync::atomic::Ordering::Relaxed) {
                    let since = blocked_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() >= stall_timeout {
                        return false;
                    }
                } else {
                    blocked_since = None;
                }
            }
        }
    }
}

impl IoSession {
    pub(super) fn start<R, E>(
        stdin: Arc<Mutex<Option<ShellStdin>>>,
        stdout: R,
        stderr: E,
        initial: Option<Vec<u8>>,
        keep_open: bool,
        budget_size: usize,
        config: &Config,
    ) -> (Self, ShellHandle, mpsc::Receiver<ShellEvent>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        E: AsyncRead + Unpin + Send + 'static,
    {
        let stop = CancellationToken::new();
        let finished = CancellationToken::new();
        let failure = Arc::new(StdMutex::new(ShellFailures::default()));
        let (tx, rx) = mpsc::channel(64);
        let handle = ShellHandle {
            inner: Arc::new(HandleInner {
                stdin: stdin.clone(),
                stop: stop.clone(),
                finished: finished.clone(),
                stdin_limit: config.limits.shell_stdin_bytes,
                write_timeout: Duration::from_millis(config.io.shell_write_timeout_ms),
            }),
        };
        // Reserve stdin ordering before returning the public handle. Initial
        // input can never be overtaken by an immediate CLI write/eof.
        let initial_guard = stdin.clone().try_lock_owned().expect("new stdin mutex");
        let initial_stop = stop.clone();
        let initial_finished = finished.clone();
        let initial_failure = failure.clone();
        let write_timeout = Duration::from_millis(config.io.shell_write_timeout_ms);

        let initial_task = tokio::spawn(async move {
            let mut guard = initial_guard;
            if let Some(bytes) = initial {
                let result = write_bytes(
                    &mut guard,
                    &bytes,
                    Instant::now() + write_timeout,
                    &initial_stop,
                    &initial_finished,
                )
                .await;
                if result.status != StdinWriteStatus::Complete && !initial_stop.is_cancelled() {
                    record_failure(
                        &initial_failure,
                        FailureKind::InitialStdin,
                        format!(
                            "initial_stdin_{:?}: {} bytes delivered",
                            result.status, result.bytes_written
                        ),
                    );
                    initial_stop.cancel();
                }
            }
            if !keep_open {
                guard.take();
            }
        });
        let progress = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sink = Arc::new(OutputSink {
            publisher: Mutex::new(Publisher {
                tx,
                sequence: 0,
                total: 0,
                raw_limit: config.limits.shell_raw_bytes,
            }),
            budget: Arc::new(Semaphore::new(budget_size)),
            chunk_size: config
                .io
                .output_batch_bytes
                .min(budget_size as u64)
                .min(READ_CHUNK as u64) as usize,
            stop: stop.clone(),
            failure: failure.clone(),
            progress: progress.clone(),
        });
        let stdout_waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stderr_waiting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stdout_task = tokio::spawn(pump(
            stdout,
            OutputStream::Stdout,
            sink.clone(),
            stdout_waiting.clone(),
        ));
        let stderr_task = tokio::spawn(pump(
            stderr,
            OutputStream::Stderr,
            sink.clone(),
            stderr_waiting.clone(),
        ));
        // Only pumps retain output senders; EOF follows their final delivery.
        drop(sink);
        (
            Self {
                stop,
                finished,
                failure,
                stdout_task,
                stderr_task,
                initial_task,
                stdout_waiting,
                stderr_waiting,
                progress,
            },
            handle,
            rx,
        )
    }

    pub(super) async fn finish(
        self,
        drain_timeout: Duration,
        process_set_confirmed: bool,
        stopped_before_exit: bool,
    ) -> (ShellState, Option<String>, bool) {
        let Self {
            stdout_task,
            stderr_task,
            initial_task,
            stdout_waiting,
            stderr_waiting,
            progress,
            finished,
            failure,
            ..
        } = self;
        let mut stdout_task = stdout_task;
        let mut stderr_task = stderr_task;
        let mut initial_task = initial_task;
        let drain = async {
            let (out, err, initial) = tokio::join!(
                pump_until_stalled(&mut stdout_task, &stdout_waiting, &progress, drain_timeout),
                pump_until_stalled(&mut stderr_task, &stderr_waiting, &progress, drain_timeout),
                timeout(drain_timeout, &mut initial_task),
            );
            out && err && initial.is_ok_and(|result| result.is_ok())
        };
        // Consumer progress resets the stall deadline; a live batch commit must
        // not be mistaken for a permanently blocked output queue.
        let drained = if process_set_confirmed {
            drain.await
        } else {
            timeout(drain_timeout, drain).await.unwrap_or(false)
        };
        if !stdout_task.is_finished() {
            stdout_task.abort();
            let _ = stdout_task.await;
        }
        if !stderr_task.is_finished() {
            stderr_task.abort();
            let _ = stderr_task.await;
        }
        if !initial_task.is_finished() {
            initial_task.abort();
            let _ = initial_task.await;
        }
        // Ensure aborted pumps release their sender/permit before terminal
        // publication. A finished JoinHandle must not be awaited twice.
        if !drained {
            record_failure(
                &failure,
                FailureKind::Drain,
                "output drain incomplete".into(),
            );
        }
        finished.cancel();
        let failure = failure.lock().expect("failure mutex");
        let reason = failure.reason();
        let output_complete = failure.output_complete(drained);
        (
            failure.completion_state(stopped_before_exit),
            reason,
            output_complete,
        )
    }
}
