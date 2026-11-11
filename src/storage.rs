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
    time::{Duration, Instant, SystemTime},
};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::{Notify, mpsc, oneshot, watch};

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
    (23, "0023_activity_bounded"),
    (24, "0024_revisit_messages"),
    (25, "0025_history_attempt_index"),
    (26, "0026_reply_expected"),
    (27, "0027_index_recovery"),
    (28, "0028_tool_group_recovery_candidates"),
    (29, "0029_active_attempt_recovery_index"),
    (30, "0030_thread_auto_compact_at"),
    (31, "0031_thread_summary_parameters"),
    (32, "0032_thread_archive"),
    (33, "0033_index_int8_scale"),
    (34, "0034_thread_tags"),
    (35, "0035_index_reclaim_lookup"),
    (36, "0036_spill_tool_metadata"),
    (37, "0037_thread_branch_kind"),
    (38, "0038_human_reply_pending"),
    (39, "0039_independent_runs"),
    (40, "0040_selection_nonappend_generation"),
    (41, "0041_receipt_reply_node_lookup"),
];
pub const SCHEMA_VERSION: i64 = MIGRATION_NAMES.len() as i64;
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;
mod migrate;
mod migration_integrity;
mod vacuum;
pub use vacuum::vacuum;
#[cfg(test)]
#[path = "storage/migration_startup_tests.rs"]
mod migration_startup_tests;
pub(crate) mod statement_observation;
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
pub(crate) enum DbJobLane {
    Foreground,
    Background,
}
impl DbJobLane {
    fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }
}
type Lane = DbJobLane;

/// In-memory view of the DB worker's active job and background no-progress state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DbWorkerActivity {
    pub(crate) active: Option<ActiveDbJob>,
    pub(crate) no_progress: Vec<NoProgressJob>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveDbJob {
    pub id: u64,
    pub kind: &'static str,
    pub lane: DbJobLane,
    pub started_at: SystemTime,
    pub started: Instant,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NoProgressJob {
    pub kind: &'static str,
    pub deferrals: u32,
}
struct ActiveJobState {
    job: ActiveDbJob,
    slow_reported: bool,
    requester: Option<&'static str>,
    counters: Option<Arc<Mutex<statement_observation::Counters>>>,
    backtrace: Option<statement_observation::SlowBacktrace>,
}
type ActiveJob = Arc<Mutex<Option<ActiveJobState>>>;

/// What the worker observes about one executed job.
#[derive(Default, Clone, Copy)]
struct JobOutcome {
    /// Counted VM steps (multiples of PROGRESS_GRANULARITY); background only.
    steps: u64,
    deferred: bool,
    background: bool,
}

type Job = Box<
    dyn FnOnce(
            &mut Connection,
            &Arc<AtomicUsize>,
            statement_observation::SlowBacktrace,
        ) -> JobOutcome
        + Send
        + 'static,
>;
enum Command {
    Run {
        job: Job,
        queued_at: Instant,
        kind: &'static str,
        lane: Lane,
        requester: Option<&'static str>,
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

#[cfg(test)]
async fn wait_for_activity(
    activity: &mut watch::Receiver<DbWorkerActivity>,
    mut matches: impl FnMut(&DbWorkerActivity) -> bool,
) -> DbWorkerActivity {
    crate::test_watchdog::watchdog("DB worker activity snapshot", async {
        loop {
            let snapshot = activity.borrow_and_update().clone();
            if matches(&snapshot) {
                return snapshot;
            }
            activity
                .changed()
                .await
                .expect("DB activity watch remains open");
        }
    })
    .await
    .expect("DB worker publishes the requested activity snapshot")
}

fn activity_snapshot(active_job: &ActiveJob, no_progress: &NoProgress) -> DbWorkerActivity {
    let active = active_job
        .lock()
        .expect("DB active-job mutex poisoned")
        .as_ref()
        .map(|active| active.job.clone());
    let mut no_progress: Vec<_> = no_progress
        .lock()
        .expect("DB no-progress mutex poisoned")
        .iter()
        .filter(|(_, count)| **count >= NO_PROGRESS_DEFERRALS)
        .map(|(kind, deferrals)| NoProgressJob {
            kind,
            deferrals: *deferrals,
        })
        .collect();
    no_progress.sort_unstable_by_key(|job| job.kind);
    DbWorkerActivity {
        active,
        no_progress,
    }
}

fn publish_activity(
    updates: &watch::Sender<DbWorkerActivity>,
    active_job: &ActiveJob,
    no_progress: &NoProgress,
) {
    updates.send_replace(activity_snapshot(active_job, no_progress));
}

struct ActiveJobGuard {
    id: u64,
    active_job: ActiveJob,
    no_progress: NoProgress,
    updates: watch::Sender<DbWorkerActivity>,
}
impl ActiveJobGuard {
    fn start(
        id: u64,
        kind: &'static str,
        lane: DbJobLane,
        started: Instant,
        active_job: ActiveJob,
        no_progress: NoProgress,
        updates: watch::Sender<DbWorkerActivity>,
    ) -> Self {
        *active_job.lock().expect("DB active-job mutex poisoned") = Some(ActiveJobState {
            job: ActiveDbJob {
                id,
                kind,
                lane,
                started_at: SystemTime::now(),
                started,
            },
            slow_reported: false,
            requester: None,
            counters: None,
            backtrace: None,
        });
        publish_activity(&updates, &active_job, &no_progress);
        Self {
            id,
            active_job,
            no_progress,
            updates,
        }
    }
}
impl Drop for ActiveJobGuard {
    fn drop(&mut self) {
        if let Ok(mut current) = self.active_job.lock()
            && current
                .as_ref()
                .is_some_and(|active| active.job.id == self.id)
        {
            *current = None;
        }
        publish_activity(&self.updates, &self.active_job, &self.no_progress);
    }
}

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
    active_job: ActiveJob,
    activity_updates: watch::Sender<DbWorkerActivity>,
    slow_limit: Arc<Mutex<SlowJobLimit>>,
    /// Committed-change signals for background maintenance requesters.
    signals: Arc<crate::maintenance::Signals>,
    checkpoint_attempts: Arc<AtomicUsize>,
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
        Self::open_internal(path, DEFAULT_QUEUE_CAPACITY, Some(owner), None, None, false).await
    }

    pub(crate) async fn open_owned_reporting(
        path: impl AsRef<Path>,
        owner: Arc<dyn Send + Sync>,
        progress: Option<tokio::sync::watch::Sender<crate::app::StartupProgress>>,
        startup_diagnostics: crate::diagnostics::Diagnostics,
        clean_shutdown: bool,
    ) -> Result<Self, StorageError> {
        Self::open_internal(
            path,
            DEFAULT_QUEUE_CAPACITY,
            Some(owner),
            progress,
            Some(startup_diagnostics),
            clean_shutdown,
        )
        .await
    }

    /// `capacity` bounds each of the foreground and background lanes.
    pub async fn open_with_capacity(
        path: impl AsRef<Path>,
        capacity: usize,
    ) -> Result<Self, StorageError> {
        Self::open_internal(path, capacity, None, None, None, false).await
    }

    async fn open_internal(
        path: impl AsRef<Path>,
        capacity: usize,
        owner: Option<Arc<dyn Send + Sync>>,
        progress: Option<tokio::sync::watch::Sender<crate::app::StartupProgress>>,
        startup_diagnostics: Option<crate::diagnostics::Diagnostics>,
        clean_shutdown: bool,
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
        let active_job: ActiveJob = Arc::new(Mutex::new(None));
        let (activity_updates, activity_receiver) = watch::channel(DbWorkerActivity::default());
        drop(activity_receiver);
        let slow_limit = Arc::new(Mutex::new(SlowJobLimit::new()));
        let next_job_id = Arc::new(AtomicU64::new(0));
        let checkpoint_attempts = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        let background_steps = Arc::new(Mutex::new(Vec::new()));
        let worker = WorkerState {
            diagnostics: diagnostics.clone(),
            foreground_waiting: foreground_waiting.clone(),
            no_progress: no_progress.clone(),
            active_job: active_job.clone(),
            activity_updates: activity_updates.clone(),
            slow_limit: slow_limit.clone(),
            next_job_id: next_job_id.clone(),
            checkpoint_attempts: checkpoint_attempts.clone(),
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
                    (progress, startup_diagnostics, clean_shutdown),
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
                active_job,
                activity_updates,
                slow_limit,
                signals: Arc::default(),
                checkpoint_attempts,
                #[cfg(test)]
                background_steps,
                join: Mutex::new(Some(join)),
                shutdown_result: Mutex::new(None),
                shutdown_done: Notify::new(),
            }),
        })
    }

    /// Observe an inspection connection without interfering with the writer queue.
    pub(crate) fn observe_inspection<T>(
        &self,
        connection: &mut Connection,
        kind: &str,
        queued_at: Instant,
        requester: Option<&'static str>,
        operation: impl FnOnce(&mut Connection) -> T,
    ) -> T {
        let started = Instant::now();
        let queue = started.saturating_duration_since(queued_at);
        let mut observation = statement_observation::StatementObservation::install(connection);
        let backtrace =
            statement_observation::SlowBacktrace::new(started, Some(observation.shared_counters()));
        let capture = backtrace.clone();
        let counters = observation
            .available()
            .then(|| observation.shared_counters());
        let observer = self.clone();
        let label = kind.to_owned();
        let mut reported = false;
        let installed = observation
            .connection()
            .progress_handler(
                PROGRESS_GRANULARITY as std::ffi::c_int,
                Some(move || {
                    let now = Instant::now();
                    capture.observe(now);
                    let elapsed = now.saturating_duration_since(started);
                    if !reported && elapsed >= SLOW_JOB_THRESHOLD {
                        reported = true;
                        if let Ok(mut limit) = observer.shared.slow_limit.try_lock()
                            && limit.admit(now)
                            && let Ok(diagnostics) = observer.shared.diagnostics.try_lock()
                            && let Some(diagnostics) = diagnostics.as_ref()
                        {
                            let snapshot = counters
                                .as_ref()
                                .and_then(|c| c.try_lock().ok().map(|c| c.snapshot()))
                                .filter(|c| c.counter_scope.is_some() || c.rows.is_some());
                            diagnostics.record_db_job_active(
                                &label,
                                "inspection",
                                elapsed,
                                snapshot,
                                capture.symbols(),
                                requester,
                            );
                        }
                    }
                    false
                }),
            )
            .is_ok();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation(observation.connection())
        }));
        if installed {
            let _ = observation
                .connection()
                .progress_handler(0, None::<fn() -> bool>);
        }
        let counters = observation
            .available()
            .then(|| observation.counters())
            .filter(|c| c.counter_scope.is_some() || c.rows.is_some());
        drop(observation);
        let run = started.elapsed();
        if (queue >= SLOW_JOB_THRESHOLD || run >= SLOW_JOB_THRESHOLD)
            && let Ok(diagnostics) = self.shared.diagnostics.try_lock()
            && let Some(diagnostics) = diagnostics.as_ref()
            && let Ok(mut limit) = self.shared.slow_limit.try_lock()
            && limit.admit(Instant::now())
        {
            diagnostics.record_db_job_with_counters(
                kind,
                queue,
                run,
                counters,
                (
                    Some("inspection"),
                    requester,
                    (run >= SLOW_JOB_THRESHOLD).then_some(self.shared.path.as_path()),
                ),
                backtrace.symbols(),
            );
        }
        match result {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Number of idle checkpoint attempts, including partial or failed attempts.
    #[doc(hidden)]
    pub fn checkpoint_attempts(&self) -> usize {
        self.shared.checkpoint_attempts.load(Ordering::SeqCst)
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

    /// Subscribe to active-job and no-progress snapshots without using the DB queue.
    pub(crate) fn subscribe_activity(&self) -> watch::Receiver<DbWorkerActivity> {
        self.shared.activity_updates.subscribe()
    }

    /// Emit one bounded diagnostic when an observed active job crosses the slow threshold.
    pub(crate) fn report_active_job_slow(&self, id: u64, now: Instant) {
        let active = {
            let Ok(mut current) = self.shared.active_job.try_lock() else {
                return;
            };
            let Some(active) = current.as_mut().filter(|active| active.job.id == id) else {
                return;
            };
            let elapsed = now.saturating_duration_since(active.job.started);
            if elapsed < SLOW_JOB_THRESHOLD || active.slow_reported {
                return;
            }
            active.slow_reported = true;
            let counters = active
                .counters
                .as_ref()
                .and_then(|c| c.try_lock().ok().map(|c| c.snapshot()))
                .filter(|c| c.counter_scope.is_some() || c.rows.is_some());
            (
                active.job.kind,
                active.job.lane,
                elapsed,
                counters,
                active.backtrace.clone(),
                active.requester,
            )
        };
        let Ok(mut limit) = self.shared.slow_limit.try_lock() else {
            return;
        };
        if !limit.admit(now) {
            return;
        }
        let Ok(diagnostics) = self.shared.diagnostics.try_lock() else {
            return;
        };
        if let Some(diagnostics) = diagnostics.as_ref() {
            diagnostics.record_db_job_active(
                active.0,
                active.1.as_str(),
                active.2,
                active.3,
                active.4.and_then(|trace| trace.symbols()),
                active.5,
            );
        }
    }

    /// Test-only access to the active worker's sampled VM counter.
    #[cfg(feature = "test-db-observation")]
    #[doc(hidden)]
    pub fn test_active_statement_counters(&self) -> Option<(u64, bool)> {
        let active = self.shared.active_job.lock().ok()?;
        let counters = active.as_ref()?.counters.as_ref()?.lock().ok()?;
        counters
            .counter_scope
            .map(|_| (counters.vm_steps, counters.sample_all_profiles))
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
        // A reserved slot is not admission. Admission is the send under this
        // lock, which `shutdown` takes before closing: a send that wins is
        // drained, one that loses is rejected with `Closed` and never runs.
        // The slot (counted by `Sender::capacity`) is released on drop (#195).
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

    pub(crate) async fn call_with_requester<T, F>(
        &self,
        requester: Option<&'static str>,
        operation: F,
    ) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let (mut command, reply) = make_job(operation);
        if let Command::Run {
            requester: slot, ..
        } = &mut command
        {
            *slot = requester;
        }
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
        self.admit_background_kind(kind, operation)
            .await?
            .receive()
            .await
    }

    /// Background admission alone: returns once the job is in the lane (or
    /// rejected), so a later shutdown is guaranteed to drain it.
    async fn admit_background_kind<T, F>(
        &self,
        kind: &'static str,
        operation: F,
    ) -> Result<DbReply<Background<T>>, StorageError>
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
        Ok(reply)
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
    let job: Job = Box::new(move |connection: &mut Connection, _, slow_backtrace| {
        let installed = connection
            .progress_handler(
                PROGRESS_GRANULARITY as std::ffi::c_int,
                Some(move || {
                    slow_backtrace.observe(Instant::now());
                    false
                }),
            )
            .is_ok();
        let mut result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation(connection)
        })) {
            Ok(result) => result,
            Err(_) => Err(StorageError::JobPanicked),
        };
        if installed {
            let _ = connection.progress_handler(0, None::<fn() -> bool>);
        }
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
            requester: None,
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
        move |connection: &mut Connection, waiting: &Arc<AtomicUsize>, slow_backtrace| {
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
                        slow_backtrace.observe(Instant::now());
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
            requester: None,
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

pub(crate) const SLOW_JOB_THRESHOLD: Duration = Duration::from_secs(1);
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
    active_job: ActiveJob,
    activity_updates: watch::Sender<DbWorkerActivity>,
    slow_limit: Arc<Mutex<SlowJobLimit>>,
    next_job_id: Arc<AtomicU64>,
    checkpoint_attempts: Arc<AtomicUsize>,
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
    startup: (
        Option<tokio::sync::watch::Sender<crate::app::StartupProgress>>,
        Option<crate::diagnostics::Diagnostics>,
        bool,
    ),
) -> Result<(), StorageError> {
    let (progress, startup_diagnostics, clean_shutdown) = startup;
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(StorageError::Worker(error.to_string())));
            return Ok(());
        }
    };
    let mut connection = match open_connection(
        &path,
        progress.as_ref(),
        startup_diagnostics.as_ref(),
        clean_shutdown,
    ) {
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
    let mut budget_limit = SlowJobLimit::new();
    let mut no_progress_limit = SlowJobLimit::new();
    while let Some(command) = lanes.next() {
        match command {
            Command::Run {
                job,
                queued_at,
                kind,
                lane,
                requester,
            } => {
                if lane == Lane::Foreground {
                    state.foreground_waiting.fetch_sub(1, Ordering::AcqRel);
                }
                lanes.background_turn = lane == Lane::Foreground;
                let started = Instant::now();
                let queue = started.duration_since(queued_at);
                let job_id = state.next_job_id.fetch_add(1, Ordering::Relaxed);
                let _activity_guard = ActiveJobGuard::start(
                    job_id,
                    kind,
                    lane,
                    started,
                    state.active_job.clone(),
                    state.no_progress.clone(),
                    state.activity_updates.clone(),
                );
                let mut observation =
                    statement_observation::StatementObservation::install(&mut connection);
                let slow_backtrace = statement_observation::SlowBacktrace::new(
                    started,
                    Some(observation.shared_counters()),
                );
                if let Ok(mut active) = state.active_job.lock()
                    && let Some(active) = active.as_mut().filter(|active| active.job.id == job_id)
                {
                    if observation.available() {
                        active.counters = Some(observation.shared_counters());
                    }
                    active.backtrace = Some(slow_backtrace.clone());
                    active.requester = requester;
                }
                let outcome = job(
                    observation.connection(),
                    &state.foreground_waiting,
                    slow_backtrace.clone(),
                );
                let counters = observation
                    .available()
                    .then(|| observation.counters())
                    .filter(|c| c.counter_scope.is_some() || c.rows.is_some());
                drop(observation);
                let run = started.elapsed();
                let diagnostics = state.diagnostics.try_lock().ok();
                let diagnostics = diagnostics.as_ref().and_then(|guard| guard.as_ref());
                let name = || kind.replace("::", ".");
                if (queue >= SLOW_JOB_THRESHOLD || run >= SLOW_JOB_THRESHOLD)
                    && let Some(diagnostics) = diagnostics
                    && let Ok(mut limit) = state.slow_limit.try_lock()
                    && limit.admit(Instant::now())
                {
                    diagnostics.record_db_job_with_counters(
                        &name(),
                        queue,
                        run,
                        counters,
                        (
                            Some(lane.as_str()),
                            requester,
                            (run >= SLOW_JOB_THRESHOLD).then_some(path.as_path()),
                        ),
                        slow_backtrace.symbols(),
                    );
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
            state.checkpoint_attempts.fetch_add(1, Ordering::SeqCst);
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

struct StartupDbTrace<'a> {
    stage: crate::app::StartupStage,
    start: Instant,
    version: Option<u32>,
    diagnostics: Option<&'a crate::diagnostics::Diagnostics>,
    progress: Option<&'a tokio::sync::watch::Sender<crate::app::StartupProgress>>,
}
impl StartupDbTrace<'_> {
    fn next(
        &mut self,
        stage: crate::app::StartupStage,
        version: Option<u32>,
        migration: Option<(u32, u32)>,
    ) {
        if let Some(diagnostics) = self.diagnostics {
            diagnostics.record_startup_stage(self.stage, self.start.elapsed(), self.version);
        }
        self.stage = stage;
        self.version = version;
        self.start = Instant::now();
        if let Some(progress) = self.progress {
            progress.send_modify(|state| {
                state.stage = stage;
                state.migration = migration;
            });
        }
    }
}
impl Drop for StartupDbTrace<'_> {
    fn drop(&mut self) {
        if let Some(diagnostics) = self.diagnostics {
            diagnostics.record_startup_stage(self.stage, self.start.elapsed(), self.version);
        }
    }
}

fn open_connection(
    path: &Path,
    progress: Option<&tokio::sync::watch::Sender<crate::app::StartupProgress>>,
    diagnostics: Option<&crate::diagnostics::Diagnostics>,
    clean_shutdown: bool,
) -> Result<Connection, StorageError> {
    use crate::app::StartupStage;
    let trace = std::cell::RefCell::new(StartupDbTrace {
        stage: StartupStage::Database,
        start: Instant::now(),
        version: None,
        diagnostics,
        progress,
    });
    register_vec()?;
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    trace
        .borrow_mut()
        .next(StartupStage::DatabasePragmas, None, None);
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
    let migrated = version > 0 && version < SCHEMA_VERSION;
    // Persist before the first migration transaction so a crash cannot bypass
    // the post-migration foreign-key scan on the next startup.
    if migrated {
        migration_integrity::persist(
            &connection,
            migration_integrity::Check::for_migrations(
                migrate::MIGRATIONS,
                version,
                SCHEMA_VERSION,
            ),
        )?;
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
    migrate::run(
        &mut connection,
        version,
        &|current, total| {
            trace.borrow_mut().next(
                StartupStage::DatabaseMigration,
                Some((version + current) as u32),
                Some((current as u32, total as u32)),
            );
        },
        &|| {
            trace
                .borrow_mut()
                .next(StartupStage::DatabaseValidate, None, None)
        },
    )?;
    validate_schema(&connection)?;
    let migration_pending = migration_integrity::pending(&connection)?;
    if let Some(scope) = &migration_pending {
        if scope.required() {
            trace
                .borrow_mut()
                .next(StartupStage::DatabaseForeignKeys, None, None);
        }
        scope.run(&connection)?;
    }
    if !clean_shutdown {
        trace
            .borrow_mut()
            .next(StartupStage::DatabaseQuickCheck, None, None);
        check_integrity(&connection, false, true)?;
    }
    if migration_pending.is_some() {
        migration_integrity::clear(&connection)?;
    }
    Ok(connection)
}

/// Run both integrity checks without starting an owner or creating a database.
pub fn manual_check(
    config: Option<&Path>,
    data_dir: Option<&Path>,
    cwd: &Path,
) -> Result<(), crate::protocol::ApiError> {
    let loaded = crate::config::Config::load_with_overrides(
        config,
        data_dir,
        cwd,
        &crate::config::PathEnvironment::current(),
    )
    .map_err(|error| {
        crate::protocol::ApiError::new(crate::protocol::ErrorCode::ConfigInvalid, error.to_string())
    })?;
    let _lock = crate::app::OwnerLock::acquire(&loaded.data_dir)?;
    register_vec().map_err(crate::app::storage_error)?;
    let connection = Connection::open_with_flags(
        loaded.data_dir.join("shellboy.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| crate::app::storage_error(StorageError::Sqlite(error)))?;
    // SQLite skips CHECK constraints in quick_check on read-only connections.
    connection
        .execute_batch("PRAGMA query_only=ON")
        .map_err(|error| crate::app::storage_error(StorageError::Sqlite(error)))?;
    check_integrity(&connection, true, true).map_err(crate::app::storage_error)
}

pub fn check_integrity(
    connection: &Connection,
    foreign_keys: bool,
    quick: bool,
) -> Result<(), StorageError> {
    if foreign_keys && connection.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(StorageError::Schema("foreign key violations".into()));
    }
    if quick {
        let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(StorageError::Schema(format!("quick_check: {integrity}")));
        }
    }
    Ok(())
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
        db.execute_batch("DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS receipt_run_completion; DROP INDEX IF EXISTS attempts_completed_scope; DROP INDEX IF EXISTS receipt_reply_pending; DROP INDEX IF EXISTS history_tool_group_attempt; DROP INDEX IF EXISTS jobs_running_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN archived_at; DROP INDEX receipt_reply_node_lookup; DELETE FROM schema_migration WHERE version>=10; DROP INDEX IF EXISTS receipt_human_reply_pending; PRAGMA user_version=9;")
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
                c.execute_batch("DROP INDEX thread_parent;DROP INDEX wait_run_state;DROP INDEX wait_recovery_thread;DROP INDEX thread_answer_latest;DROP INDEX receipt_thread_order;DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS receipt_run_completion; DROP INDEX IF EXISTS attempts_completed_scope; DROP INDEX IF EXISTS receipt_reply_pending; DROP INDEX IF EXISTS history_tool_group_attempt; DROP INDEX IF EXISTS jobs_running_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN archived_at; DROP INDEX receipt_reply_node_lookup; DELETE FROM schema_migration WHERE version>=16;DROP INDEX shell_started_readback;DROP INDEX shell_owner_started_readback;DROP INDEX compact_operation_thread;DROP INDEX automatic_compaction_turn;DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS receipt_run_completion; DROP INDEX IF EXISTS attempts_completed_scope; DELETE FROM schema_migration WHERE version=15;DROP INDEX IF EXISTS receipt_human_reply_pending; PRAGMA user_version=14;PRAGMA ignore_check_constraints=ON;INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('corrupt','fixture','{}','hash','failed','malformed','2026-10-31T00:00:00Z');PRAGMA ignore_check_constraints=OFF;").unwrap();
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
                db.execute_batch(r#"DROP INDEX thread_parent;DROP INDEX wait_run_state;DROP INDEX wait_recovery_thread;DROP INDEX thread_answer_latest;DROP INDEX receipt_thread_order;DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS receipt_run_completion; DROP INDEX IF EXISTS attempts_completed_scope; DROP INDEX IF EXISTS receipt_reply_pending; DROP INDEX IF EXISTS history_tool_group_attempt; DROP INDEX IF EXISTS jobs_running_recovery; ALTER TABLE thread DROP COLUMN auto_compact_at; ALTER TABLE thread DROP COLUMN segment_chars; ALTER TABLE thread DROP COLUMN reduce_rounds; DROP INDEX IF EXISTS threads_visible_updated; DROP INDEX IF EXISTS threads_archived_updated; DROP TABLE thread_tag; DROP INDEX threads_cwd_updated; DROP TRIGGER logical_index_embedding_chunk_insert; DROP TRIGGER logical_index_embedding_chunk_delete; DROP TRIGGER logical_index_embedding_chunk_update; DROP TRIGGER logical_index_space_insert; DROP TRIGGER logical_index_space_delete; DROP TRIGGER logical_index_space_update; DROP INDEX chunks_job; ALTER TABLE index_space DROP COLUMN int8_scale; ALTER TABLE thread DROP COLUMN archived_at; DROP INDEX receipt_reply_node_lookup; DELETE FROM schema_migration WHERE version>=16;DROP INDEX IF EXISTS receipt_human_reply_pending; PRAGMA user_version=15;INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/a','p','m','now');INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('r','t','completed','2000','2001');INSERT INTO event(id,kind,source_json,target_thread_id,payload_ref_json,created_at)VALUES('e','child_recovery_wait','{}','t','{"thread_id":"t","run_id":"r","turn_id":"u"}','2000');"#).unwrap();
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn active_job_snapshot_can_be_read_while_the_writer_is_busy() {
        crate::test_watchdog::watchdog("active DB job snapshot", async {
            let root = tempfile::tempdir().unwrap();
            let diagnostics = crate::diagnostics::Diagnostics::start(
                root.path().join("logs"),
                crate::config::Logging::default(),
            );
            let db = DbWorker::open(root.path().join("db")).await.unwrap();
            db.attach_diagnostics(diagnostics.clone());
            let (started_tx, started_rx) = oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let job_db = db.clone();
            let job = tokio::spawn(async move {
                let private_text = String::from("private conversation test marker");
                job_db
                    .call(move |connection| {
                        assert!(!private_text.is_empty());
                        let value: i64 =
                            connection.query_row("SELECT ?1", [42], |row| row.get(0))?;
                        assert_eq!(value, 42);
                        let _ = started_tx.send(());
                        release_rx.recv().unwrap();
                        Ok::<_, StorageError>(())
                    })
                    .await
            });
            started_rx.await.unwrap();
            let mut activity_rx = db.subscribe_activity();
            let snapshot =
                wait_for_activity(&mut activity_rx, |snapshot| snapshot.active.is_some()).await;
            let active = snapshot.active.as_ref().expect("active job snapshot");
            assert!(active.kind.contains("active_job_snapshot_can_be_read"));
            assert_eq!(active.lane, DbJobLane::Foreground);
            assert!(snapshot.no_progress.is_empty());
            assert!(active.started_at <= SystemTime::now());

            db.report_active_job_slow(active.id, active.started + SLOW_JOB_THRESHOLD);
            release_tx.send(()).unwrap();
            job.await.unwrap().unwrap();
            let completed = wait_for_activity(&mut activity_rx, |activity| {
                activity.active.is_none() && activity.no_progress.is_empty()
            })
            .await;
            assert!(completed.active.is_none());
            assert!(completed.no_progress.is_empty());
            db.shutdown().await.unwrap();
            diagnostics.shutdown().await;

            let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
            assert!(!log.contains("private conversation test marker"), "{log}");
            let rows: Vec<serde_json::Value> = log
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let active_logs: Vec<_> = rows
                .iter()
                .filter(|row| row["event"] == "db_job_active")
                .collect();
            assert_eq!(active_logs.len(), 1, "{log}");
            assert!(
                active_logs[0]["db_job_active"]["kind"]
                    .as_str()
                    .unwrap()
                    .contains("active_job_snapshot_can_be_read")
            );
            assert_eq!(active_logs[0]["db_job_active"]["lane"], "foreground");
            assert_eq!(
                active_logs[0]["db_job_active"]["statement_counters"]["rows"],
                1
            );
            assert_eq!(
                active_logs[0]["db_job_active"]["statement_counters"]["row_scope"],
                "job_cumulative"
            );
            assert!(
                active_logs[0]["db_job_active"]["statement_counters"]["vm_steps"]
                    .as_u64()
                    .is_some_and(|steps| steps > 0),
                "unit-test worker must observe even fast statements: {log}"
            );
            assert!(
                active_logs[0]["db_job_active"]["elapsed_ms"]
                    .as_u64()
                    .unwrap()
                    >= 1000
            );
            assert_eq!(
                active_logs[0]["db_job_active"].as_object().unwrap().len(),
                4
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn writer_job_retains_requester_category() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = crate::diagnostics::Diagnostics::start(
            root.path().join("logs"),
            crate::config::Logging::default(),
        );
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        db.attach_diagnostics(diagnostics.clone());
        db.call_with_requester(Some("shell"), |_| {
            std::thread::sleep(SLOW_JOB_THRESHOLD + Duration::from_millis(20));
            Ok::<_, StorageError>(())
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
        diagnostics.shutdown().await;
        let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
        let event: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(event["db_job_slow"]["requester"], "shell");
        assert_eq!(event["db_job_slow"]["lane"], "foreground");
    }

    #[tokio::test]
    async fn inspection_active_event_is_emitted_once() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = crate::diagnostics::Diagnostics::start(
            root.path().join("logs"),
            crate::config::Logging::default(),
        );
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        db.attach_diagnostics(diagnostics.clone());
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .create_scalar_function(
                "test_pause",
                0,
                rusqlite::functions::FunctionFlags::empty(),
                |_| {
                    std::thread::sleep(SLOW_JOB_THRESHOLD + Duration::from_millis(20));
                    Ok(1_i64)
                },
            )
            .unwrap();
        db.observe_inspection(&mut connection, "HistoryList", Instant::now(), Some("shell"), |conn| {
            conn.query_row("SELECT test_pause()", [], |r| r.get::<_, i64>(0)).unwrap();
            conn.query_row("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<10000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0)).unwrap();
            conn.query_row("SELECT test_pause()", [], |r| r.get::<_, i64>(0)).unwrap();
        });
        db.shutdown().await.unwrap();
        diagnostics.shutdown().await;
        let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
        let active: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|row| row["event"] == "db_job_active")
            .collect();
        assert_eq!(active.len(), 1, "{log}");
        assert_eq!(active[0]["db_job_active"]["kind"], "HistoryList");
        assert_eq!(active[0]["db_job_active"]["lane"], "inspection");
        assert_eq!(active[0]["db_job_active"]["requester"], "shell");
        let observed = &active[0]["db_job_active"]["statement_counters"];
        assert_eq!(observed["sql_scope"], "overrun_statement");
        assert!(observed["sql"].as_str().unwrap().contains("WITH RECURSIVE"));
        let slow = log
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|row| row["event"] == "db_job_slow")
            .unwrap();
        assert_eq!(
            observed["sql"],
            slow["db_job_slow"]["statement_counters"]["sql"]
        );
        assert!(!log.contains("test_pause"));
    }

    #[tokio::test]
    async fn inspection_event_has_counters_and_never_logs_private_values() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = crate::diagnostics::Diagnostics::start(
            root.path().join("logs"),
            crate::config::Logging::default(),
        );
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        db.attach_diagnostics(diagnostics.clone());
        let mut connection = Connection::open_in_memory().unwrap();
        let queued = Instant::now() - SLOW_JOB_THRESHOLD;
        let value: String = db.observe_inspection(
            &mut connection,
            "HistoryGet",
            queued,
            Some("external"),
            |conn| {
                conn.query_row("SELECT ?1", ["private inspection marker"], |row| row.get(0))
                    .unwrap()
            },
        );
        assert_eq!(value, "private inspection marker");
        db.shutdown().await.unwrap();
        diagnostics.shutdown().await;
        let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
        assert!(!log.contains("private inspection marker"));
        let event: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(event["db_job_slow"]["kind"], "HistoryGet");
        assert_eq!(event["db_job_slow"]["lane"], "inspection");
        assert_eq!(event["db_job_slow"]["requester"], "external");
        assert_eq!(event["db_job_slow"]["statement_counters"]["rows"], 1);
        assert_eq!(
            event["db_job_slow"]["statement_counters"]["row_scope"],
            "job_cumulative"
        );
    }

    #[tokio::test]
    async fn slow_sql_with_embedded_message_does_not_reach_log() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = crate::diagnostics::Diagnostics::start(
            root.path().join("logs"),
            crate::config::Logging::default(),
        );
        let db = DbWorker::open(root.path().join("db")).await.unwrap();
        db.attach_diagnostics(diagnostics.clone());
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .create_scalar_function(
                "test_delay",
                0,
                rusqlite::functions::FunctionFlags::empty(),
                |_| {
                    std::thread::sleep(Duration::from_millis(5));
                    Ok("ok")
                },
            )
            .unwrap();
        let message = "private_message_body_marker";
        let sql = format!("SELECT test_delay() || '{message}'");
        let value = db.observe_inspection(
            &mut connection,
            "HistoryGet",
            Instant::now() - SLOW_JOB_THRESHOLD,
            Some("shell"),
            |conn| {
                conn.query_row(&sql, [], |row| row.get::<_, String>(0))
                    .unwrap()
            },
        );
        assert!(value.contains(message));
        db.shutdown().await.unwrap();
        diagnostics.shutdown().await;
        let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
        assert!(!log.contains(message), "{log}");
        let event: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert!(
            event["db_job_slow"]["statement_counters"]["sql"]
                .as_str()
                .is_some(),
            "{log}"
        );
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
        let mut activity_rx = db.subscribe_activity();
        let active_snapshot = activity_rx.borrow_and_update().clone();
        let active = active_snapshot
            .active
            .as_ref()
            .expect("dequeued job is visible");
        assert!(active.kind.contains("index_next_delay"), "{}", active.kind);
        assert_eq!(active.lane, DbJobLane::Foreground);
        assert!(active.started_at <= SystemTime::now());
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
        assert_eq!(rows[0]["db_job_slow"]["lane"], "foreground");
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
                .all(|r| r["db_job_slow"]["statement_counters"]["rows"] == 0)
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
        // The worker records a unit after replying; shutdown orders the read.
        db.shutdown().await.unwrap();
        let steps = db.take_background_steps();
        assert!(steps[0].1 > BACKGROUND_STEP_BUDGET, "{steps:?}");
        assert!(!steps[0].2);
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
            // Admit b2 strictly before b3 and both before shutdown. Awaiting
            // admission itself, not lane capacity, which already drops when a
            // slot is reserved but not yet sent (#195).
            for name in ["b2", "b3"] {
                let order = order.clone();
                let reply = db
                    .admit_background_kind("test", move |_| {
                        order.lock().unwrap().push(name);
                        Ok(())
                    })
                    .await
                    .unwrap();
                waits.push(tokio::spawn(
                    async move { reply.receive().await.map(|_| ()) },
                ));
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
            let mut activity_rx = db.subscribe_activity();
            let _ = activity_rx.borrow_and_update();
            assert!(matches!(stuck_unit(&db, true).await, Background::Deferred));
            assert!(
                db.background_no_progress().is_empty(),
                "one deferral is not evidence"
            );
            let after_first = wait_for_activity(&mut activity_rx, |activity| {
                activity.active.is_none() && activity.no_progress.is_empty()
            })
            .await;
            assert!(after_first.no_progress.is_empty());
            assert!(matches!(stuck_unit(&db, true).await, Background::Deferred));
            let stuck = db.background_no_progress();
            assert_eq!(stuck.len(), 1);
            assert!(stuck[0].0.contains("stuck_unit"), "{stuck:?}");
            assert_eq!(stuck[0].1, NO_PROGRESS_DEFERRALS);
            let activity = wait_for_activity(&mut activity_rx, |activity| {
                activity.no_progress.len() == 1
                    && activity.no_progress[0].kind == stuck[0].0
                    && activity.active.is_none()
            })
            .await;
            assert_eq!(activity.no_progress[0].deferrals, NO_PROGRESS_DEFERRALS);
            // A completed job of the same kind is progress and clears the state.
            assert!(matches!(stuck_unit(&db, false).await, Background::Done(())));
            assert!(db.background_no_progress().is_empty());
            let cleared = wait_for_activity(&mut activity_rx, |activity| {
                activity.active.is_none() && activity.no_progress.is_empty()
            })
            .await;
            assert!(cleared.no_progress.is_empty());
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

#[cfg(test)]
mod integrity_policy_tests {
    use super::*;

    #[test]
    fn reopen_obeys_clean_marker_and_unclean_checks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        drop(open_connection(&path, None, None, false).unwrap());
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES('corrupt','fixture','{}','hash','failed','malformed','now');").unwrap();
        drop(db);
        assert!(open_connection(&path, None, None, true).is_ok());
        assert!(
            open_connection(&path, None, None, false)
                .unwrap_err()
                .to_string()
                .contains("quick_check")
        );
    }

    #[test]
    fn pending_migration_rechecks_foreign_keys_even_after_version_is_current() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        drop(open_connection(&path, None, None, false).unwrap());
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO app_meta(key,value) VALUES('integrity.migration_pending','1'); INSERT INTO run(id,thread_id,state,created_at) VALUES('orphan','missing','completed','now');").unwrap();
        drop(db);
        assert!(
            open_connection(&path, None, None, true)
                .unwrap_err()
                .to_string()
                .contains("foreign key")
        );
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM app_meta WHERE key='integrity.migration_pending'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn integrity_policy_respects_selected_checks() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(parent_id INTEGER REFERENCES parent(id), n INTEGER CHECK(n > 0)); INSERT INTO child VALUES(99, 1);").unwrap();
        assert!(check_integrity(&db, false, false).is_ok());
        assert!(
            check_integrity(&db, true, false)
                .unwrap_err()
                .to_string()
                .contains("foreign key")
        );
        assert!(check_integrity(&db, false, true).is_ok());
        db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE child SET n=0; PRAGMA ignore_check_constraints=OFF;")
            .unwrap();
        assert!(
            check_integrity(&db, false, true)
                .unwrap_err()
                .to_string()
                .contains("quick_check")
        );
    }
}

#[cfg(test)]
#[path = "storage/selection_generation_tests.rs"]
mod selection_generation_tests;
