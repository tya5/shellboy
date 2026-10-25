//! Ownership of the existing bounded pipe tasks, independent of native process cleanup.
use super::*;

pub(super) struct IoSession {
    pub(super) stop: CancellationToken,
    finished: CancellationToken,
    pub(super) failure: Arc<StdMutex<ShellFailures>>,
    stdout_task: tokio::task::JoinHandle<bool>,
    stderr_task: tokio::task::JoinHandle<bool>,
    initial_task: tokio::task::JoinHandle<()>,
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
        });
        let stdout_task = tokio::spawn(pump(stdout, OutputStream::Stdout, sink.clone()));
        let stderr_task = tokio::spawn(pump(stderr, OutputStream::Stderr, sink.clone()));
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
            },
            handle,
            rx,
        )
    }

    pub(super) async fn finish(
        self,
        drain_timeout: Duration,
        stopped_before_exit: bool,
    ) -> (ShellState, Option<String>, bool) {
        let Self {
            stdout_task,
            stderr_task,
            initial_task,
            finished,
            failure,
            ..
        } = self;
        let mut stdout_task = stdout_task;
        let mut stderr_task = stderr_task;
        let mut initial_task = initial_task;
        let drained = timeout(drain_timeout, async {
            let (out, err, initial) =
                tokio::join!(&mut stdout_task, &mut stderr_task, &mut initial_task);
            out.is_ok_and(|v| v) && err.is_ok_and(|v| v) && initial.is_ok()
        })
        .await
        .unwrap_or(false);
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
