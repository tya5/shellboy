//! One SQLite connection on a dedicated OS thread, with bounded async admission.
//!
//! Hold the data-directory owner lock until [`DbWorker::shutdown`] completes.
//! Jobs must perform only short database work, never file/network/process waits.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{Notify, mpsc, oneshot};

pub(crate) const MAX_VECTOR_DIMENSIONS: usize = 8192;

pub(crate) const MIGRATION_NAMES: &[(i64, &str)] = &[
    (1, "0001_initial"),
    (2, "0002_gc_indexes"),
    (3, "0003_context_indexes"),
    (4, "0004_attachment_provenance"),
    (5, "0005_origin_credentials"),
    (6, "0006_receipt_budget"),
    (7, "0007_usage_and_source_indexes"),
    (8, "0008_receipt_accounting_compat"),
    (9, "0009_compaction_turn_indexes"),
    (10, "0010_hot_query_indexes"),
    (11, "0011_current_index_metadata"),
    (12, "0012_revisit_metadata"),
    (13, "0013_message_path_admission"),
    (14, "0014_turn_interrupt"),
    (15, "0015_readback_indexes"),
    (16, "0016_thread_observation"),
    (17, "0017_thread_parent"),
    (18, "0018_summary_usage_metadata"),
    (19, "0019_context_attempt_scope"),
    (20, "0020_gc_index_source"),
    (21, "0021_event_retention"),
    (22, "0022_logical_usage_counters"),
];
pub const SCHEMA_VERSION: i64 = MIGRATION_NAMES.len() as i64;
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;
mod migrate;
pub(crate) const CURRENT_SCHEMA: &str = include_str!("storage/current.sql");

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{0}")]
    Operation(Box<crate::protocol::ApiError>),
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema is not supported: {0}; existing files were preserved")]
    Schema(String),
    #[error("database configuration: {0}")]
    Configuration(String),
    #[error("database worker has shut down")]
    Closed,
    #[error("database worker queue is full; operation was not accepted")]
    QueueFull,
    #[error("input preparation: {0}")]
    InvalidInput(String),
    #[error("database worker failed: {0}")]
    Worker(String),
    #[error("database closure panicked; its open transaction was rolled back")]
    JobPanicked,
    #[error(
        "database closure returned without committing its transaction; transaction was rolled back"
    )]
    UncommittedTransaction,
}

impl From<crate::protocol::ApiError> for StorageError {
    fn from(error: crate::protocol::ApiError) -> Self {
        Self::Operation(Box::new(error))
    }
}

impl StorageError {
    /// Preserve domain failures across DB work without turning them into fatal
    /// persistence failures. Missing rows are classified at their query site.
    pub fn into_api_error(self) -> crate::protocol::ApiError {
        use crate::protocol::{ApiError, ErrorCode};
        match self {
            Self::Operation(error) => *error,
            Self::InvalidInput(message) => ApiError::new(ErrorCode::InvalidState, message),
            error @ (Self::QueueFull | Self::Closed) => {
                ApiError::new(ErrorCode::InvalidState, error.to_string())
            }
            error => save_error(error.to_string())
                .hint("Check storage capacity and permissions; existing data was preserved"),
        }
    }
}

/// Common classification for genuine source-store failures; callers retain
/// responsibility for diagnostic context and redaction of the message.
pub(crate) fn save_error(message: impl Into<String>) -> crate::protocol::ApiError {
    crate::protocol::ApiError::new(crate::protocol::ErrorCode::SaveFailed, message)
}

/// Step budget of one background unit while a foreground job waits: the
/// smallest power of two at or above the largest healthy unit measured by
/// `maintenance::step_tests` (87,040 steps, managed-image retention, on the
/// synthetic large fixture; storage design #171). Not a time bound.
pub(crate) const BACKGROUND_STEP_BUDGET: u64 = 1 << 17;
/// Progress-handler interval in VM steps; overshoot is at most one interval.
pub(crate) const PROGRESS_GRANULARITY: u64 = 1024;

/// Outcome of a background job. `Deferred` means the job was interrupted at its
/// step budget while foreground work waited and its open transaction rolled
/// back; the caller resumes later from committed state. It is not a failure.
#[derive(Debug)]
pub enum Background<T> {
    Done(T),
    Deferred,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    Foreground,
    Background,
}

/// What the worker observes about one executed job.
#[derive(Default, Clone, Copy)]
struct JobOutcome {
    /// Counted VM steps (multiples of PROGRESS_GRANULARITY); background only.
    steps: u64,
    deferred: bool,
    background: bool,
}

type Job = Box<dyn FnOnce(&mut Connection, &Arc<AtomicUsize>) -> JobOutcome + Send + 'static>;
enum Command {
    Run {
        job: Job,
        queued_at: Instant,
        kind: &'static str,
        lane: Lane,
    },
    Shutdown,
}

struct Lanes {
    foreground: mpsc::Sender<Command>,
    background: mpsc::Sender<Command>,
}
impl Lanes {
    fn get(&self, lane: Lane) -> &mpsc::Sender<Command> {
        match lane {
            Lane::Foreground => &self.foreground,
            Lane::Background => &self.background,
        }
    }
}

/// Consecutive deferrals per background kind, with no completed job of that
/// kind in between. Bounded by the finite set of background call sites.
type NoProgress = Arc<Mutex<HashMap<&'static str, u32>>>;
/// Test-only record of (kind, counted steps, deferred) per background unit.
#[cfg(test)]
type StepLog = Arc<Mutex<Vec<(&'static str, u64, bool)>>>;
/// A kind is reported after this many consecutive deferrals. Every deferral
/// already consumed the full step budget, and the retry starts from the same
/// committed state, so a second one shows the unit does not fit one slot.
pub(crate) const NO_PROGRESS_DEFERRALS: u32 = 2;

struct Shared {
    path: PathBuf,
    diagnostics: Arc<Mutex<Option<crate::diagnostics::Diagnostics>>>,
    /// Taking these senders closes admission across every clone before shutdown.
    sender: Mutex<Option<Lanes>>,
    /// Admitted foreground jobs not yet taken by the worker. The background
    /// progress handler reads it to decide whether to interrupt.
    foreground_waiting: Arc<AtomicUsize>,
    #[cfg_attr(not(test), allow(dead_code))]
    no_progress: NoProgress,
    /// Committed-change signals for background maintenance requesters.
    signals: Arc<crate::maintenance::Signals>,
    #[cfg(test)]
    background_steps: StepLog,
    join: Mutex<Option<thread::JoinHandle<Result<(), StorageError>>>>,
    shutdown_result: Mutex<Option<Result<(), String>>>,
    shutdown_done: Notify,
}

/// Cloneable admission handle; all clones share one connection and shutdown gate.
#[derive(Clone)]
pub struct DbWorker {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for DbWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbWorker").finish_non_exhaustive()
    }
}

/// A request which has been admitted without waiting for a queue slot.
/// Dropping it cancels observation only; the admitted operation still runs.
pub struct DbReply<T> {
    receiver: oneshot::Receiver<Result<T, StorageError>>,
}
impl<T> DbReply<T> {
    pub async fn receive(self) -> Result<T, StorageError> {
        self.receiver.await.map_err(|_| StorageError::Closed)?
    }
}

impl DbWorker {
    /// Open only after acquiring the data-directory owner lock.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Self::open_with_capacity(path, DEFAULT_QUEUE_CAPACITY).await
    }

    /// Retain the owner lock on the writer thread even if every caller is dropped.
    /// The guard is released only after SQLite has actually closed.
    pub async fn open_owned(
        path: impl AsRef<Path>,
        owner: Arc<dyn Send + Sync>,
    ) -> Result<Self, StorageError> {
        Self::open_internal(path, DEFAULT_QUEUE_CAPACITY, Some(owner)).await
    }

    /// `capacity` bounds each of the foreground and background lanes.
    pub async fn open_with_capacity(
        path: impl AsRef<Path>,
        capacity: usize,
    ) -> Result<Self, StorageError> {
        Self::open_internal(path, capacity, None).await
    }

    async fn open_internal(
        path: impl AsRef<Path>,
        capacity: usize,
        owner: Option<Arc<dyn Send + Sync>>,
    ) -> Result<Self, StorageError> {
        if capacity == 0 {
            return Err(StorageError::Configuration(
                "queue capacity must be positive".into(),
            ));
        }
        let path = path.as_ref().to_path_buf();
        let stored_path = path.clone();
        let (foreground, foreground_receiver) = mpsc::channel(capacity);
        let (background, background_receiver) = mpsc::channel(capacity);
        let (ready_send, ready_receive) = oneshot::channel();
        let diagnostics = Arc::new(Mutex::new(None));
        let foreground_waiting = Arc::new(AtomicUsize::new(0));
        let no_progress: NoProgress = Arc::new(Mutex::new(HashMap::new()));
        #[cfg(test)]
        let background_steps = Arc::new(Mutex::new(Vec::new()));
        let worker = WorkerState {
            diagnostics: diagnostics.clone(),
            foreground_waiting: foreground_waiting.clone(),
            no_progress: no_progress.clone(),
            #[cfg(test)]
            background_steps: background_steps.clone(),
        };
        let join = thread::Builder::new()
            .name("shellboy-db".into())
            .spawn(move || {
                let _owner_guard = owner;
                run_worker(
                    path,
                    foreground_receiver,
                    background_receiver,
                    ready_send,
                    worker,
                )
            })
            .map_err(|error| StorageError::Worker(error.to_string()))?;
        let ready = ready_receive
            .await
            .map_err(|_| StorageError::Worker("worker exited during open".into()))?;
        if let Err(error) = ready {
            // Joining never blocks a UI or async worker OS thread.
            let _ = tokio::task::spawn_blocking(move || join.join()).await;
            return Err(error);
        }
        Ok(Self {
            shared: Arc::new(Shared {
                path: stored_path,
                diagnostics,
                sender: Mutex::new(Some(Lanes {
                    foreground,
                    background,
                })),
                foreground_waiting,
                no_progress,
                signals: Arc::default(),
                #[cfg(test)]
                background_steps,
                join: Mutex::new(Some(join)),
                shutdown_result: Mutex::new(None),
                shutdown_done: Notify::new(),
            }),
        })
    }

    /// The path used to open this worker, including custom database filenames.
    pub(crate) fn path(&self) -> &Path {
        &self.shared.path
    }

    pub(crate) fn attach_diagnostics(&self, diagnostics: crate::diagnostics::Diagnostics) {
        *self
            .shared
            .diagnostics
            .lock()
            .expect("DB diagnostics mutex poisoned") = Some(diagnostics);
    }

    /// Producers mark committed changes here; the maintenance requester waits on it.
    pub fn signals(&self) -> &Arc<crate::maintenance::Signals> {
        &self.shared.signals
    }

    /// Background kinds currently deferred at least [`NO_PROGRESS_DEFERRALS`]
    /// consecutive times without a completed job of the same kind in between.
    /// Read-only state for a later status surface; bounded by call sites.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn background_no_progress(&self) -> Vec<(&'static str, u32)> {
        let mut kinds: Vec<_> = self
            .shared
            .no_progress
            .lock()
            .expect("DB no-progress mutex poisoned")
            .iter()
            .filter(|(_, count)| **count >= NO_PROGRESS_DEFERRALS)
            .map(|(kind, count)| (*kind, *count))
            .collect();
        kinds.sort_unstable();
        kinds
    }

    #[cfg(test)]
    pub(crate) fn take_background_steps(&self) -> Vec<(&'static str, u64, bool)> {
        std::mem::take(&mut *self.shared.background_steps.lock().unwrap())
    }

    async fn admit(&self, lane: Lane, mut command: Command) -> Result<(), StorageError> {
        let sender = self
            .shared
            .sender
            .lock()
            .expect("DB admission mutex poisoned")
            .as_ref()
            .map(|lanes| lanes.get(lane).clone())
            .ok_or(StorageError::Closed)?;
        let permit = sender
            .reserve_owned()
            .await
            .map_err(|_| StorageError::Closed)?;
        let admission = self
            .shared
            .sender
            .lock()
            .expect("DB admission mutex poisoned");
        if admission.is_none() {
            return Err(StorageError::Closed);
        }
        // Queue time excludes the asynchronous wait for a lane slot.
        if let Command::Run { queued_at, .. } = &mut command {
            *queued_at = Instant::now();
        }
        if lane == Lane::Foreground {
            // Counted before the worker can take it; the worker decrements.
            self.shared
                .foreground_waiting
                .fetch_add(1, Ordering::AcqRel);
        }
        permit.send(command);
        Ok(())
    }

    /// Foreground lane. Wait asynchronously for bounded admission, then await the
    /// durable result. A cancelled caller does not cancel an admitted transaction.
    pub async fn call<T, F>(&self, operation: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let (command, reply) = make_job(operation);
        self.admit(Lane::Foreground, command).await?;
        reply.receive().await
    }

    /// Background lane, reached only through [`BackgroundRequester::unit`]:
    /// work that no request awaits and that can be rolled back and retried
    /// from committed state (storage design #171).
    /// Once [`BACKGROUND_STEP_BUDGET`] is exceeded while a foreground job
    /// waits, SQLite is interrupted, nothing more commits, the open
    /// transaction rolls back and the result is [`Background::Deferred`].
    #[cfg(test)]
    async fn call_background<T, F>(&self, operation: F) -> Result<Background<T>, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        self.call_background_kind(job_kind::<F>(), operation).await
    }

    async fn call_background_kind<T, F>(
        &self,
        kind: &'static str,
        operation: F,
    ) -> Result<Background<T>, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let (mut command, reply) = make_background_job(operation);
        if let Command::Run { kind: named, .. } = &mut command {
            // The requester's closure, not the wrapper, names the kind.
            *named = kind;
        }
        self.admit(Lane::Background, command).await?;
        reply.receive().await
    }

    /// Nonblocking foreground admission for callers which must preserve input when full.
    pub fn try_call<T, F>(&self, operation: F) -> Result<DbReply<T>, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let admission = self
            .shared
            .sender
            .lock()
            .expect("DB admission mutex poisoned");
        let sender = &admission.as_ref().ok_or(StorageError::Closed)?.foreground;
        let (command, reply) = make_job(operation);
        self.shared
            .foreground_waiting
            .fetch_add(1, Ordering::AcqRel);
        sender.try_send(command).map_err(|error| {
            self.shared
                .foreground_waiting
                .fetch_sub(1, Ordering::AcqRel);
            match error {
                mpsc::error::TrySendError::Full(_) => StorageError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => StorageError::Closed,
            }
        })?;
        Ok(reply)
    }

    /// Close admission across every clone, drain accepted jobs in both lanes,
    /// close SQLite, and join the writer before the caller releases the
    /// data-directory owner lock. The actual shutdown is spawned so dropping
    /// this future cannot strand it.
    pub async fn shutdown(&self) -> Result<(), StorageError> {
        let lanes = self
            .shared
            .sender
            .lock()
            .expect("DB admission mutex poisoned")
            .take();
        if let Some(lanes) = lanes {
            let shared = self.shared.clone();
            tokio::spawn(async move {
                let _ = lanes.foreground.send(Command::Shutdown).await;
                drop(lanes);
                let join = shared.join.lock().expect("DB join mutex poisoned").take();
                let outcome = match join {
                    Some(join) => match tokio::task::spawn_blocking(move || join.join()).await {
                        Ok(Ok(result)) => result.map_err(|error| error.to_string()),
                        Ok(Err(_)) => Err("database worker panicked".into()),
                        Err(error) => Err(error.to_string()),
                    },
                    None => Err("database writer join handle missing".into()),
                };
                *shared
                    .shutdown_result
                    .lock()
                    .expect("DB result mutex poisoned") = Some(outcome);
                shared.shutdown_done.notify_waiters();
            });
        }
        loop {
            // Register before checking the result to avoid a lost notification.
            let notified = self.shared.shutdown_done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self
                .shared
                .shutdown_result
                .lock()
                .expect("DB result mutex poisoned")
                .as_ref()
            {
                return result.clone().map_err(StorageError::Worker);
            }
            notified.await;
        }
    }
}

/// The declared row or batch bound of one background unit. The closure must
/// apply it as `LIMIT` on an indexed path (storage design #171, rule 1).
#[derive(Clone, Copy, Debug)]
pub struct Limit(std::num::NonZeroU32);
impl Limit {
    pub fn get(self) -> i64 {
        i64::from(self.0.get())
    }
    pub fn rows(self) -> usize {
        self.0.get() as usize
    }
}

/// The only way to issue background DB work. Each requester owns exactly one
/// handle; it is not `Clone` and `unit` takes `&mut self`, so a requester has
/// at most one outstanding request (storage design #171, rule 2).
pub struct BackgroundRequester {
    database: DbWorker,
}
impl std::fmt::Debug for BackgroundRequester {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundRequester")
            .finish_non_exhaustive()
    }
}
impl BackgroundRequester {
    pub fn new(database: DbWorker) -> Self {
        Self { database }
    }
    /// Run one declared bounded unit on the background lane.
    pub async fn unit<T, F>(
        &mut self,
        limit: std::num::NonZeroU32,
        operation: F,
    ) -> Result<Background<T>, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, Limit) -> Result<T, StorageError> + Send + 'static,
    {
        let limit = Limit(limit);
        self.database
            .call_background_kind(job_kind::<F>(), move |connection| {
                operation(connection, limit)
            })
            .await
    }
}

fn finish_transaction<T>(connection: &Connection, result: &mut Result<T, StorageError>) {
    if !connection.is_autocommit() {
        if let Err(error) = connection.execute_batch("ROLLBACK") {
            *result = Err(error.into());
        } else if result.is_ok() {
            *result = Err(StorageError::UncommittedTransaction);
        }
    }
}

fn make_job<T, F>(operation: F) -> (Command, DbReply<T>)
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    let job: Job = Box::new(move |connection: &mut Connection, _| {
        let mut result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation(connection)
        })) {
            Ok(result) => result,
            Err(_) => Err(StorageError::JobPanicked),
        };
        finish_transaction(connection, &mut result);
        let _ = sender.send(result);
        JobOutcome::default()
    });
    (
        Command::Run {
            job,
            queued_at: Instant::now(),
            kind: job_kind::<F>(),
            lane: Lane::Foreground,
        },
        DbReply { receiver },
    )
}

/// Budget state shared by the progress handler and commit hook of one job.
#[derive(Default)]
struct StepBudget {
    ticks: AtomicU64,
    hit: AtomicBool,
}

fn make_background_job<T, F>(operation: F) -> (Command, DbReply<Background<T>>)
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    let job: Job = Box::new(
        move |connection: &mut Connection, waiting: &Arc<AtomicUsize>| {
            let budget = Arc::new(StepBudget::default());
            let (handler_budget, hook_budget) = (budget.clone(), budget.clone());
            let waiting = waiting.clone();
            let limit = BACKGROUND_STEP_BUDGET.div_ceil(PROGRESS_GRANULARITY);
            // Sticky: after the hit every later step is interrupted and every
            // later COMMIT (explicit or autocommit) becomes a rollback, even if
            // the closure swallows the interrupt error.
            let installed = connection
                .progress_handler(
                    PROGRESS_GRANULARITY as std::ffi::c_int,
                    Some(move || {
                        if handler_budget.hit.load(Ordering::Acquire) {
                            return true;
                        }
                        let ticks = handler_budget.ticks.fetch_add(1, Ordering::AcqRel) + 1;
                        if ticks > limit && waiting.load(Ordering::Acquire) > 0 {
                            handler_budget.hit.store(true, Ordering::Release);
                            return true;
                        }
                        false
                    }),
                )
                .and_then(|()| {
                    connection.commit_hook(Some(move || hook_budget.hit.load(Ordering::Acquire)))
                });
            let mut result = match installed {
                Err(error) => Err(StorageError::from(error)),
                Ok(()) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    operation(connection)
                })) {
                    Ok(result) => result,
                    Err(_) => Err(StorageError::JobPanicked),
                },
            };
            // Remove both hooks before the cleanup ROLLBACK so it cannot be interrupted.
            let _ = connection.progress_handler(0, None::<fn() -> bool>);
            let _ = connection.commit_hook(None::<fn() -> bool>);
            finish_transaction(connection, &mut result);
            let deferred = budget.hit.load(Ordering::Acquire);
            let steps = budget.ticks.load(Ordering::Acquire) * PROGRESS_GRANULARITY;
            let reply = if deferred {
                Ok(Background::Deferred)
            } else {
                result.map(Background::Done)
            };
            let _ = sender.send(reply);
            JobOutcome {
                steps,
                deferred,
                background: true,
            }
        },
    );
    (
        Command::Run {
            job,
            queued_at: Instant::now(),
            kind: job_kind::<F>(),
            lane: Lane::Background,
        },
        DbReply { receiver },
    )
}

// Closure type names are compiler-provided source identifiers, not captured values.
fn job_kind<F>() -> &'static str {
    let name = std::any::type_name::<F>();
    let name = name.split("::{{closure}}").next().unwrap_or(name);
    name.trim_start_matches("shellboy::")
}

const SLOW_JOB_THRESHOLD: Duration = Duration::from_secs(1);
const SLOW_JOB_WINDOW: Duration = Duration::from_secs(60);
const SLOW_JOBS_PER_WINDOW: u8 = 10;

struct SlowJobLimit {
    window: Instant,
    emitted: u8,
}
impl SlowJobLimit {
    fn new() -> Self {
        Self {
            window: Instant::now(),
            emitted: 0,
        }
    }
    fn admit(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window) >= SLOW_JOB_WINDOW {
            self.window = now;
            self.emitted = 0;
        }
        if self.emitted >= SLOW_JOBS_PER_WINDOW {
            return false;
        }
        self.emitted += 1;
        true
    }
}

// This is an idle opportunity throttle, not a periodic worker or durability timer.
const IDLE_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

struct IdleCheckpoint {
    last_attempt: Instant,
    checkpointed_changes: u64,
}

impl IdleCheckpoint {
    fn new(now: Instant, changes: u64) -> Self {
        Self {
            last_attempt: now,
            checkpointed_changes: changes,
        }
    }

    fn due(&self, now: Instant, changes: u64) -> bool {
        changes != self.checkpointed_changes
            && now.duration_since(self.last_attempt) >= IDLE_CHECKPOINT_INTERVAL
    }

    fn attempted(&mut self, now: Instant, changes: u64, complete: bool) {
        self.last_attempt = now;
        if complete {
            self.checkpointed_changes = changes;
        }
    }
}

struct WorkerState {
    diagnostics: Arc<Mutex<Option<crate::diagnostics::Diagnostics>>>,
    foreground_waiting: Arc<AtomicUsize>,
    no_progress: NoProgress,
    #[cfg(test)]
    background_steps: StepLog,
}

/// Two bounded lanes taken in strict alternation when both hold work, so a
/// foreground job waits for at most one background unit and background work
/// still advances under sustained foreground load.
struct LaneReceivers {
    foreground: mpsc::Receiver<Command>,
    background: mpsc::Receiver<Command>,
    foreground_open: bool,
    background_open: bool,
    /// The previous job was foreground, so a waiting background job goes next.
    background_turn: bool,
    /// Blocks only the DB OS thread; no I/O or timer driver is enabled.
    runtime: tokio::runtime::Runtime,
}

impl LaneReceivers {
    fn try_lane(&mut self, lane: Lane) -> Option<Command> {
        let (receiver, open) = match lane {
            Lane::Foreground => (&mut self.foreground, &mut self.foreground_open),
            Lane::Background => (&mut self.background, &mut self.background_open),
        };
        if !*open {
            return None;
        }
        match receiver.try_recv() {
            Ok(command) => Some(command),
            Err(mpsc::error::TryRecvError::Empty) => None,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *open = false;
                None
            }
        }
    }

    fn next(&mut self) -> Option<Command> {
        loop {
            let order = if self.background_turn {
                [Lane::Background, Lane::Foreground]
            } else {
                [Lane::Foreground, Lane::Background]
            };
            for lane in order {
                if let Some(command) = self.try_lane(lane) {
                    return Some(command);
                }
            }
            if !self.foreground_open && !self.background_open {
                return None;
            }
            let (foreground_open, background_open) = (self.foreground_open, self.background_open);
            let (foreground, background) = (&mut self.foreground, &mut self.background);
            let woke = self.runtime.block_on(async {
                tokio::select! {
                    biased;
                    command = foreground.recv(), if foreground_open => (Lane::Foreground, command),
                    command = background.recv(), if background_open => (Lane::Background, command),
                }
            });
            match woke {
                (_, Some(command)) => return Some(command),
                (Lane::Foreground, None) => self.foreground_open = false,
                (Lane::Background, None) => self.background_open = false,
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.foreground.is_empty() && self.background.is_empty()
    }

    fn close(&mut self) {
        self.foreground.close();
        self.background.close();
    }
}

fn run_worker(
    path: PathBuf,
    foreground: mpsc::Receiver<Command>,
    background: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<(), StorageError>>,
    state: WorkerState,
) -> Result<(), StorageError> {
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(StorageError::Worker(error.to_string())));
            return Ok(());
        }
    };
    let mut connection = match open_connection(&path) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    if ready.send(Ok(())).is_err() {
        return Ok(());
    }
    let mut lanes = LaneReceivers {
        foreground,
        background,
        foreground_open: true,
        background_open: true,
        background_turn: false,
        runtime,
    };
    let mut checkpoint = IdleCheckpoint::new(Instant::now(), connection.total_changes());
    let mut slow_limit = SlowJobLimit::new();
    let mut budget_limit = SlowJobLimit::new();
    let mut no_progress_limit = SlowJobLimit::new();
    while let Some(command) = lanes.next() {
        match command {
            Command::Run {
                job,
                queued_at,
                kind,
                lane,
            } => {
                if lane == Lane::Foreground {
                    state.foreground_waiting.fetch_sub(1, Ordering::AcqRel);
                }
                lanes.background_turn = lane == Lane::Foreground;
                let started = Instant::now();
                let queue = started.duration_since(queued_at);
                let outcome = job(&mut connection, &state.foreground_waiting);
                let run = started.elapsed();
                let diagnostics = state.diagnostics.try_lock().ok();
                let diagnostics = diagnostics.as_ref().and_then(|guard| guard.as_ref());
                let name = || kind.replace("::", ".");
                if (queue >= SLOW_JOB_THRESHOLD || run >= SLOW_JOB_THRESHOLD)
                    && let Some(diagnostics) = diagnostics
                    && slow_limit.admit(Instant::now())
                {
                    diagnostics.record_db_job(&name(), queue, run);
                }
                if outcome.background {
                    #[cfg(test)]
                    state.background_steps.lock().unwrap().push((
                        kind,
                        outcome.steps,
                        outcome.deferred,
                    ));
                    let deferrals = {
                        let mut map = state
                            .no_progress
                            .lock()
                            .expect("DB no-progress mutex poisoned");
                        if outcome.deferred {
                            let count = map.entry(kind).or_insert(0);
                            *count = count.saturating_add(1);
                            *count
                        } else {
                            map.remove(kind);
                            0
                        }
                    };
                    if outcome.deferred
                        && let Some(diagnostics) = diagnostics
                    {
                        if budget_limit.admit(Instant::now()) {
                            diagnostics.record_db_budget(&name(), outcome.steps, queue, run);
                        }
                        if deferrals == NO_PROGRESS_DEFERRALS
                            && no_progress_limit.admit(Instant::now())
                        {
                            diagnostics.record_db_no_progress(&name(), deferrals);
                        }
                    }
                }
            }
            Command::Shutdown => {
                // Admission is already closed; run every admitted job in both
                // lanes before the final checkpoint and close.
                lanes.close();
                continue;
            }
        }
        let now = Instant::now();
        let changes = connection.total_changes();
        if lanes.is_empty() && checkpoint.due(now, changes) {
            // PASSIVE never waits for readers. Partial/failed attempts stay dirty
            // but cannot spin on every job; shutdown still checks real errors.
            let complete = connection
                .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    let busy = row.get::<_, i64>(0)?;
                    let frames = row.get::<_, i64>(1)?;
                    let checkpointed = row.get::<_, i64>(2)?;
                    Ok(busy == 0 && frames >= 0 && frames == checkpointed)
                })
                .unwrap_or(false);
            checkpoint.attempted(now, changes, complete);
        }
    }
    // A concurrent reader may keep an older snapshot, preventing truncation.
    // That status does not invalidate committed WAL frames or the writer close.
    // Actual checkpoint/close errors still propagate as storage failures.
    let _busy = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    })?;
    connection
        .close()
        .map_err(|(_, error)| StorageError::Sqlite(error))
}

pub(crate) fn register_vec() -> Result<(), StorageError> {
    static REGISTERED: OnceLock<i32> = OnceLock::new();
    let status = *REGISTERED.get_or_init(|| {
        // SAFETY: this pinned sqlite-vec function is the SQLite extension entry
        // point ABI. Upstream's Rust binding declares it without arguments; the
        // explicit transmute restores SQLite's documented callback signature.
        // It is statically linked into the same bundled SQLite, never unloaded,
        // and registration occurs once before this worker opens any connection.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::ffi::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::ffi::c_int,
            >(
                sqlite_vec::sqlite3_vec_init as *const ()
            )))
        }
    });
    if status != rusqlite::ffi::SQLITE_OK {
        return Err(StorageError::Configuration(format!(
            "sqlite-vec registration returned {status}"
        )));
    }
    Ok(())
}

fn open_connection(path: &Path) -> Result<Connection, StorageError> {
    register_vec()?;
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let vec_version: String = connection.query_row("SELECT vec_version()", [], |row| row.get(0))?;
    if vec_version != "v0.1.9" {
        return Err(StorageError::Configuration(format!(
            "expected sqlite-vec v0.1.9, found {vec_version}"
        )));
    }
    // Validate before changing persistent journal mode on an unknown database.
    let version = schema_version(&connection)?;
    validate_ledger(&connection, version)?;
    let journal: String = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(StorageError::Configuration(format!(
            "WAL required, found journal_mode={journal}"
        )));
    }
    connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    let synchronous: i64 = connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if synchronous != 2 || foreign_keys != 1 {
        return Err(StorageError::Configuration(format!(
            "FULL/FK required, found synchronous={synchronous}, foreign_keys={foreign_keys}"
        )));
    }
    let version = if version == 0 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(CURRENT_SCHEMA)?;
        transaction.commit()?;
        SCHEMA_VERSION
    } else {
        version
    };
    migrate::run(&mut connection, version)?;
    validate_schema(&connection)?;
    if connection.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(StorageError::Schema("foreign key violations".into()));
    }
    let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(StorageError::Schema(format!("quick_check: {integrity}")));
    }
    Ok(connection)
}

fn validate_schema(connection: &Connection) -> Result<i64, StorageError> {
    let version = schema_version(connection)?;
    if version != SCHEMA_VERSION {
        return Err(StorageError::Schema(format!(
            "user_version {version} requires migration to {SCHEMA_VERSION}; open with the current writer first"
        )));
    }
    validate_ledger(connection, version)?;
    Ok(version)
}

fn schema_version(connection: &Connection) -> Result<i64, StorageError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(StorageError::Schema(format!(
            "user_version {version}; supported {SCHEMA_VERSION}"
        )));
    }
    if version == 0 {
        let objects: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if objects != 0 {
            return Err(StorageError::Schema(
                "unversioned nonempty database; automatic adoption is prohibited".into(),
            ));
        }
    }
    Ok(version)
}

/// Ledger validation is general schema bookkeeping, independent of upgrades.
fn validate_ledger(connection: &Connection, version: i64) -> Result<(), StorageError> {
    if version == 0 {
        return Ok(());
    }
    let ledger = connection
        .prepare("SELECT version,name FROM schema_migration ORDER BY version")
        .map_err(|_| StorageError::Schema("migration ledger missing".into()))?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ledger
        .iter()
        .map(|(version, name)| (*version, name.as_str()))
        .ne(MIGRATION_NAMES[..version as usize].iter().copied())
    {
        return Err(StorageError::Schema(
            "migration ledger disagrees with user_version".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod checkpoint_tests {
    use super::{IDLE_CHECKPOINT_INTERVAL, IdleCheckpoint};
    use std::time::Instant;

    #[test]
    fn idle_checkpoint_requires_activity_and_throttles_partial_or_failed_attempts() {
        let start = Instant::now();
        let mut checkpoint = IdleCheckpoint::new(start, 10);
        let due = start + IDLE_CHECKPOINT_INTERVAL;
        assert!(!checkpoint.due(due, 10));
        assert!(!checkpoint.due(due - std::time::Duration::from_nanos(1), 11));
        assert!(checkpoint.due(due, 11));
        // Both a blocked reader and an SQLite error leave activity pending.
        checkpoint.attempted(due, 11, false);
        assert!(!checkpoint.due(due, 11));
        let retry = due + IDLE_CHECKPOINT_INTERVAL;
        assert!(checkpoint.due(retry, 11));
        checkpoint.attempted(retry, 11, true);
        assert!(!checkpoint.due(retry + IDLE_CHECKPOINT_INTERVAL, 11));
        assert!(checkpoint.due(retry + IDLE_CHECKPOINT_INTERVAL, 12));
    }
}

#[cfg(test)]
mod schema_validation_tests {
    use super::*;

    #[test]
    fn current_bootstrap_and_validation_do_not_depend_on_upgrade_module() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(CURRENT_SCHEMA).unwrap();
        assert_eq!(validate_schema(&db).unwrap(), SCHEMA_VERSION);
        db.execute_batch("DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS attempts_completed_scope; DELETE FROM schema_migration WHERE version>=10; PRAGMA user_version=9;")
            .unwrap();
        assert!(
            matches!(validate_schema(&db), Err(StorageError::Schema(message)) if message.contains("current writer"))
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            9
        );
    }
}

#[cfg(test)]
mod readback_migration_tests {
    #[tokio::test]
    async fn schema14_index_upgrade_keeps_source_rows_and_malformed_optional_receipts() {
        crate::test_watchdog::watchdog("schema14 derived index upgrade",async {
            let root=tempfile::tempdir().unwrap();let path=root.path().join("db");
            {
                let c=rusqlite::Connection::open(&path).unwrap();c.execute_batch(super::CURRENT_SCHEMA).unwrap();
                c.execute_batch("DROP INDEX thread_parent;DROP INDEX wait_run_state;DROP INDEX wait_recovery_thread;DROP INDEX thread_answer_latest;DROP INDEX receipt_thread_order;DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS attempts_completed_scope; DELETE FROM schema_migration WHERE version>=16;DROP INDEX shell_started_readback;DROP INDEX shell_owner_started_readback;DROP INDEX compact_operation_thread;DROP INDEX automatic_compaction_turn;DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS attempts_completed_scope; DELETE FROM schema_migration WHERE version=15;PRAGMA user_version=14;PRAGMA ignore_check_constraints=ON;INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('corrupt','fixture','{}','hash','failed','malformed','2026-10-31T00:00:00Z');PRAGMA ignore_check_constraints=OFF;").unwrap();
                c.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('keep','/','','p','m','2026-10-31T00:00:00Z')",[]).unwrap();
            }
            let failure=match super::DbWorker::open(&path).await {Err(error)=>error,Ok(worker)=>{worker.shutdown().await.unwrap();panic!("existing CHECK corruption must still reject owner startup")}};
            assert!(failure.to_string().contains("quick_check"),"{failure}");
            let c=rusqlite::Connection::open(&path).unwrap();
            assert_eq!(c.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0)).unwrap(),super::SCHEMA_VERSION);
            assert_eq!(c.query_row("SELECT result_ref_json FROM operation_receipt WHERE operation_id='corrupt'",[],|r|r.get::<_,String>(0)).unwrap(),"malformed");
            assert_eq!(c.query_row("SELECT count(*) FROM thread WHERE id='keep'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
            assert_eq!(c.query_row("SELECT count(*) FROM schema_migration",[],|r|r.get::<_,i64>(0)).unwrap(),super::SCHEMA_VERSION);
        }).await.expect("derived index upgrade watchdog");
    }
}

#[cfg(test)]
mod thread_observation_migration_tests {
    #[tokio::test]
    async fn schema15_observation_indexes_preserve_saved_source_identity_and_reopen() {
        crate::test_watchdog::watchdog("schema15 Thread observation upgrade",async {
            let root=tempfile::tempdir().unwrap();let path=root.path().join("db");
            {
                let db=rusqlite::Connection::open(&path).unwrap();db.execute_batch(super::CURRENT_SCHEMA).unwrap();
                db.execute_batch(r#"DROP INDEX thread_parent;DROP INDEX wait_run_state;DROP INDEX wait_recovery_thread;DROP INDEX thread_answer_latest;DROP INDEX receipt_thread_order;DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS attempts_completed_scope; DELETE FROM schema_migration WHERE version>=16;PRAGMA user_version=15;INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now');INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2000','2001');INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('e','child_recovery_wait','{}','t','{"thread_id":"t","run_id":"r","turn_id":"u"}','2000');"#).unwrap();
            }
            for _ in 0..2 {
                let worker=super::DbWorker::open(&path).await.unwrap();
                worker.call(|db| {
                    assert_eq!(db.query_row("SELECT rowid FROM run WHERE id='r'",[],|r|r.get::<_,i64>(0))?,1);
                    assert_eq!(db.query_row("SELECT rowid FROM event WHERE id='e'",[],|r|r.get::<_,i64>(0))?,1);
                    assert_eq!(db.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0))?,super::SCHEMA_VERSION);
                    assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('wait_run_state','wait_recovery_thread','thread_answer_latest','receipt_thread_order')",[],|r|r.get::<_,i64>(0))?,4);
                    Ok(())
                }).await.unwrap();worker.shutdown().await.unwrap();
            }
        }).await.unwrap();
    }
}

#[cfg(test)]
mod slow_job_tests {
    use super::*;

    #[test]
    fn emission_limit_is_bounded_and_resets() {
        let start = Instant::now();
        let mut limit = SlowJobLimit {
            window: start,
            emitted: 0,
        };
        for _ in 0..SLOW_JOBS_PER_WINDOW {
            assert!(limit.admit(start));
        }
        assert!(!limit.admit(start));
        assert!(!limit.admit(start + SLOW_JOB_WINDOW - Duration::from_nanos(1)));
        assert!(limit.admit(start + SLOW_JOB_WINDOW));
    }

    async fn index_next_delay(
        db: &DbWorker,
        gate: std::sync::mpsc::Receiver<()>,
        started: oneshot::Sender<()>,
    ) -> Result<(), StorageError> {
        let private_text = String::from("private conversation test marker");
        db.call(move |_| {
            assert!(!private_text.is_empty());
            let _ = started.send(());
            gate.recv().unwrap();
            Ok(())
        })
        .await
    }

    fn gc_collect(db: &DbWorker) -> Result<DbReply<()>, StorageError> {
        db.try_call(|_| Ok(()))
    }

    #[tokio::test]
    async fn slow_execution_and_queue_are_observed_with_distinct_caller_names() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = crate::diagnostics::Diagnostics::start(
            root.path().join("logs"),
            crate::config::Logging::default(),
        );
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        db.attach_diagnostics(diagnostics.clone());
        db.call(|_| Ok::<_, StorageError>(())).await.unwrap();
        let (start_tx, start_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_db = db.clone();
        let first =
            tokio::spawn(async move { index_next_delay(&first_db, release_rx, start_tx).await });
        start_rx.await.unwrap();
        let queued: Vec<_> = (0..SLOW_JOBS_PER_WINDOW + 2)
            .map(|_| gc_collect(&db).unwrap())
            .collect();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        release_tx.send(()).unwrap();
        first.await.unwrap().unwrap();
        for reply in queued {
            reply.receive().await.unwrap();
        }
        db.shutdown().await.unwrap();
        diagnostics.shutdown().await;
        let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
        let rows: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), usize::from(SLOW_JOBS_PER_WINDOW), "{log}");
        assert!(!log.contains("private conversation test marker"));
        assert_eq!(rows[0]["event"], "db_job_slow");
        assert!(
            rows[0]["db_job_slow"]["kind"]
                .as_str()
                .unwrap()
                .contains("index_next_delay"),
            "{log}"
        );
        assert!(rows[0]["db_job_slow"]["run_ms"].as_u64().unwrap() >= 1000);
        assert!(
            rows[1]["db_job_slow"]["kind"]
                .as_str()
                .unwrap()
                .contains("gc_collect"),
            "{log}"
        );
        assert!(rows[1]["db_job_slow"]["queue_ms"].as_u64().unwrap() >= 1000);
        assert!(
            rows.iter()
                .all(|r| r["db_job_slow"].as_object().unwrap().len() == 3)
        );
    }
}

#[cfg(test)]
mod lane_budget_tests {
    use super::*;
    use std::sync::mpsc as sync_mpsc;

    /// Far beyond the budget: unbounded without interruption (watchdog-protected).
    const ENDLESS: &str =
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c) SELECT count(*) FROM c";
    const BUDGET_TICKS: u64 = BACKGROUND_STEP_BUDGET.div_ceil(PROGRESS_GRANULARITY);

    async fn open(root: &tempfile::TempDir) -> DbWorker {
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        db.call(|db| {
            db.execute_batch("CREATE TEMP TABLE marker(x)")?;
            Ok(())
        })
        .await
        .unwrap();
        db
    }

    /// Writes a marker, waits until the test has admitted foreground work,
    /// then runs an endless query, swallows the interrupt and tries to commit
    /// both inside its transaction and with an autocommit write.
    fn heavy_background(
        db: &DbWorker,
        started: oneshot::Sender<()>,
        go: sync_mpsc::Receiver<()>,
    ) -> tokio::task::JoinHandle<Result<Background<()>, StorageError>> {
        let db = db.clone();
        tokio::spawn(async move {
            db.call_background(move |db| {
                let tx = db.transaction()?;
                tx.execute("INSERT INTO marker VALUES(1)", [])?;
                let _ = started.send(());
                go.recv().unwrap();
                let swallowed = tx.query_row(ENDLESS, [], |r| r.get::<_, i64>(0));
                assert!(swallowed.is_err());
                let _ = tx.commit();
                let _ = db.execute("INSERT INTO marker VALUES(2)", []);
                Ok(())
            })
            .await
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn foreground_waits_for_at_most_one_background_unit_and_nothing_commits_after_the_hit() {
        crate::test_watchdog::watchdog("foreground behind heavy background", async {
            let root = tempfile::tempdir().unwrap();
            let db = open(&root).await;
            let (started_tx, started_rx) = oneshot::channel();
            let (go_tx, go_rx) = sync_mpsc::channel();
            let background = heavy_background(&db, started_tx, go_rx);
            started_rx.await.unwrap();
            // Admitted (and counted as waiting) before the heavy query starts.
            let foreground = db
                .try_call(|db| {
                    Ok(db.query_row("SELECT count(*) FROM marker", [], |r| r.get::<_, i64>(0))?)
                })
                .unwrap();
            go_tx.send(()).unwrap();
            assert!(matches!(
                background.await.unwrap().unwrap(),
                Background::Deferred
            ));
            assert_eq!(
                foreground.receive().await.unwrap(),
                0,
                "no write survived the hit"
            );
            let steps = db.take_background_steps();
            assert_eq!(steps.len(), 1);
            let (_, counted, deferred) = steps[0];
            assert!(deferred);
            // The whole background unit the foreground job waited behind.
            assert_eq!(counted, (BUDGET_TICKS + 1) * PROGRESS_GRANULARITY);
            assert!(counted <= BACKGROUND_STEP_BUDGET + PROGRESS_GRANULARITY);
            db.shutdown().await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn background_over_budget_runs_to_completion_without_foreground_demand() {
        let root = tempfile::tempdir().unwrap();
        let db = open(&root).await;
        let rows = (BACKGROUND_STEP_BUDGET * 8) as i64;
        let outcome = db
            .call_background(move |db| {
                Ok(db.query_row(
                    "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<?1) SELECT count(*) FROM c",
                    [rows / 8],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert!(matches!(outcome, Background::Done(n) if n == rows / 8));
        let steps = db.take_background_steps();
        assert!(steps[0].1 > BACKGROUND_STEP_BUDGET, "{steps:?}");
        assert!(!steps[0].2);
        db.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lanes_alternate_and_shutdown_drains_both() {
        crate::test_watchdog::watchdog("lane alternation", async {
            let root = tempfile::tempdir().unwrap();
            let db = open(&root).await;
            let order = Arc::new(Mutex::new(Vec::new()));
            let (started_tx, started_rx) = oneshot::channel();
            let (go_tx, go_rx) = sync_mpsc::channel::<()>();
            let first = {
                let (db, order) = (db.clone(), order.clone());
                tokio::spawn(async move {
                    db.call_background(move |_| {
                        let _ = started_tx.send(());
                        go_rx.recv().unwrap();
                        order.lock().unwrap().push("b1");
                        Ok(())
                    })
                    .await
                })
            };
            started_rx.await.unwrap();
            let mut waits = Vec::new();
            for name in ["f1", "f2"] {
                let order = order.clone();
                waits.push(tokio::spawn(
                    db.try_call(move |_| {
                        order.lock().unwrap().push(name);
                        Ok(())
                    })
                    .unwrap()
                    .receive(),
                ));
            }
            for name in ["b2", "b3"] {
                let (db, order) = (db.clone(), order.clone());
                waits.push(tokio::spawn(async move {
                    db.call_background(move |_| {
                        order.lock().unwrap().push(name);
                        Ok(())
                    })
                    .await
                    .map(|_| ())
                }));
            }
            // Admission is asynchronous for background callers; wait for both.
            while db
                .shared
                .sender
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .background
                .capacity()
                > DEFAULT_QUEUE_CAPACITY - 2
            {
                tokio::task::yield_now().await;
            }
            go_tx.send(()).unwrap();
            // Shutdown is requested while jobs are queued; every one still runs.
            db.shutdown().await.unwrap();
            assert!(matches!(
                first.await.unwrap().unwrap(),
                Background::Done(())
            ));
            for wait in waits {
                wait.await.unwrap().unwrap();
            }
            assert_eq!(*order.lock().unwrap(), ["b1", "f1", "b2", "f2", "b3"]);
        })
        .await
        .unwrap();
    }

    async fn stuck_unit(db: &DbWorker, endless: bool) -> Background<()> {
        let (started_tx, started_rx) = oneshot::channel();
        let (go_tx, go_rx) = sync_mpsc::channel();
        let job = {
            let db = db.clone();
            tokio::spawn(async move {
                db.call_background(move |db| {
                    let _ = started_tx.send(());
                    go_rx.recv().unwrap();
                    if endless {
                        db.query_row(ENDLESS, [], |r| r.get::<_, i64>(0))?;
                    }
                    Ok(())
                })
                .await
            })
        };
        started_rx.await.unwrap();
        let foreground = db.try_call(|_| Ok(())).unwrap();
        go_tx.send(()).unwrap();
        foreground.receive().await.unwrap();
        job.await.unwrap().unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn repeated_deferral_is_reported_as_bounded_no_progress_state() {
        crate::test_watchdog::watchdog("no-progress detection", async {
            let root = tempfile::tempdir().unwrap();
            let diagnostics = crate::diagnostics::Diagnostics::start(
                root.path().join("logs"),
                crate::config::Logging::default(),
            );
            let db = open(&root).await;
            db.attach_diagnostics(diagnostics.clone());
            assert!(matches!(stuck_unit(&db, true).await, Background::Deferred));
            assert!(
                db.background_no_progress().is_empty(),
                "one deferral is not evidence"
            );
            assert!(matches!(stuck_unit(&db, true).await, Background::Deferred));
            let stuck = db.background_no_progress();
            assert_eq!(stuck.len(), 1);
            assert!(stuck[0].0.contains("stuck_unit"), "{stuck:?}");
            assert_eq!(stuck[0].1, NO_PROGRESS_DEFERRALS);
            // A completed job of the same kind is progress and clears the state.
            assert!(matches!(stuck_unit(&db, false).await, Background::Done(())));
            assert!(db.background_no_progress().is_empty());
            db.shutdown().await.unwrap();
            diagnostics.shutdown().await;
            let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
            let rows: Vec<serde_json::Value> = log
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            let budget: Vec<_> = rows
                .iter()
                .filter(|r| r["event"] == "db_job_budget")
                .collect();
            assert_eq!(budget.len(), 2, "{log}");
            assert_eq!(budget[0]["db_job_budget"].as_object().unwrap().len(), 4);
            let no_progress: Vec<_> = rows
                .iter()
                .filter(|r| r["event"] == "db_job_no_progress")
                .collect();
            assert_eq!(no_progress.len(), 1, "{log}");
            assert_eq!(no_progress[0]["db_job_no_progress"]["deferrals"], 2);
        })
        .await
        .unwrap();
    }
}
