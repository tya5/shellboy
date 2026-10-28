//! One SQLite connection on a dedicated OS thread, with bounded async admission.
//!
//! Hold the data-directory owner lock until [`DbWorker::shutdown`] completes.
//! Jobs must perform only short database work, never file/network/process waits.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
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

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;
enum Command {
    Run(Job),
    Shutdown,
}

struct Shared {
    path: PathBuf,
    /// Taking this sender closes admission across every clone before shutdown.
    sender: Mutex<Option<mpsc::Sender<Command>>>,
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
        let (sender, receiver) = mpsc::channel(capacity);
        let (ready_send, ready_receive) = oneshot::channel();
        let join = thread::Builder::new()
            .name("shellboy-db".into())
            .spawn(move || {
                let _owner_guard = owner;
                run_worker(path, receiver, ready_send)
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
                sender: Mutex::new(Some(sender)),
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

    /// Wait asynchronously for bounded admission, then await the durable result.
    /// A cancelled caller does not cancel an already admitted transaction.
    pub async fn call<T, F>(&self, operation: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let sender = self
            .shared
            .sender
            .lock()
            .expect("DB admission mutex poisoned")
            .as_ref()
            .cloned()
            .ok_or(StorageError::Closed)?;
        let permit = sender
            .reserve_owned()
            .await
            .map_err(|_| StorageError::Closed)?;
        let (command, reply) = make_job(operation);
        {
            let admission = self
                .shared
                .sender
                .lock()
                .expect("DB admission mutex poisoned");
            if admission.is_none() {
                return Err(StorageError::Closed);
            }
            permit.send(command);
        }
        reply.receive().await
    }

    /// Nonblocking admission for callers which must preserve input when full.
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
        let sender = admission.as_ref().ok_or(StorageError::Closed)?;
        let (command, reply) = make_job(operation);
        sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => StorageError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => StorageError::Closed,
        })?;
        Ok(reply)
    }

    /// Close admission across every clone, drain accepted jobs, close SQLite, and
    /// join the writer before the caller releases the data-directory owner lock.
    /// The actual shutdown is spawned so dropping this future cannot strand it.
    pub async fn shutdown(&self) -> Result<(), StorageError> {
        let sender = self
            .shared
            .sender
            .lock()
            .expect("DB admission mutex poisoned")
            .take();
        if let Some(sender) = sender {
            let shared = self.shared.clone();
            tokio::spawn(async move {
                let _ = sender.send(Command::Shutdown).await;
                drop(sender);
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

fn make_job<T, F>(operation: F) -> (Command, DbReply<T>)
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    let job = Box::new(move |connection: &mut Connection| {
        let mut result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation(connection)
        })) {
            Ok(result) => result,
            Err(_) => Err(StorageError::JobPanicked),
        };
        if !connection.is_autocommit() {
            if let Err(error) = connection.execute_batch("ROLLBACK") {
                result = Err(error.into());
            } else if result.is_ok() {
                result = Err(StorageError::UncommittedTransaction);
            }
        }
        let _ = sender.send(result);
    });
    (Command::Run(job), DbReply { receiver })
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

fn run_worker(
    path: PathBuf,
    mut receiver: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<(), StorageError>>,
) -> Result<(), StorageError> {
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
    let mut checkpoint = IdleCheckpoint::new(Instant::now(), connection.total_changes());
    while let Some(command) = receiver.blocking_recv() {
        match command {
            Command::Run(job) => job(&mut connection),
            Command::Shutdown => break,
        }
        let now = Instant::now();
        let changes = connection.total_changes();
        if receiver.is_empty() && checkpoint.due(now, changes) {
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
        db.execute_batch("DELETE FROM schema_migration WHERE version>=10; PRAGMA user_version=9;")
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
