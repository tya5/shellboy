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
];
pub const SCHEMA_VERSION: i64 = MIGRATION_NAMES.len() as i64;
pub const DEFAULT_QUEUE_CAPACITY: usize = 128;
const HOT_QUERY_INDEX_MIGRATION: &str = include_str!("../migrations/0010_hot_query_indexes.sql");
const INITIAL_MIGRATION: &str = include_str!("../migrations/0001_initial.sql");
const GC_INDEX_MIGRATION: &str = include_str!("../migrations/0002_gc_indexes.sql");
const ATTACHMENT_PROVENANCE_MIGRATION: &str =
    include_str!("../migrations/0004_attachment_provenance.sql");
const ORIGIN_CREDENTIAL_MIGRATION: &str = include_str!("../migrations/0005_origin_credentials.sql");
const RECEIPT_BUDGET_MIGRATION: &str = include_str!("../migrations/0006_receipt_budget.sql");
const USAGE_SOURCE_INDEX_MIGRATION: &str =
    include_str!("../migrations/0007_usage_and_source_indexes.sql");
const RECEIPT_ACCOUNTING_COMPAT_MIGRATION: &str =
    include_str!("../migrations/0008_receipt_accounting_compat.sql");
const COMPACTION_TURN_INDEX_MIGRATION: &str =
    include_str!("../migrations/0009_compaction_turn_indexes.sql");
const CONTEXT_INDEX_MIGRATION: &str = include_str!("../migrations/0003_context_indexes.sql");

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
    let version = validate_schema(&connection)?;
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
    if version == 0 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(INITIAL_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 2 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(GC_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 3 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(CONTEXT_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 4 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(ATTACHMENT_PROVENANCE_MIGRATION)?;
        migrate_attachment_sources(&transaction)?;
        transaction.commit()?;
    }
    if version < 5 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(ORIGIN_CREDENTIAL_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 6 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(RECEIPT_BUDGET_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 7 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(USAGE_SOURCE_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 8 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(RECEIPT_ACCOUNTING_COMPAT_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 9 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(COMPACTION_TURN_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
    if version < 10 {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(HOT_QUERY_INDEX_MIGRATION)?;
        transaction.commit()?;
    }
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

/// Preserve known user provenance; unknown legacy origins stay pending and are
/// never silently attached to the newest human message.
fn migrate_attachment_sources(tx: &rusqlite::Transaction<'_>) -> Result<(), StorageError> {
    use rusqlite::params;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    let rows=tx.prepare("SELECT a.id,a.source_node_id,n.kind,n.payload_json FROM attachment_ref a LEFT JOIN node n ON n.id=a.source_node_id ORDER BY a.created_at,a.id")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (attachment, source, kind, raw) in rows {
        let selections=tx.prepare("SELECT thread_id,reason FROM attachment_selection WHERE attachment_id=?1 ORDER BY thread_id")?.query_map([&attachment],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
        // Compaction already retired these source slots. Preserve their
        // tombstones instead of manufacturing a pending source for replay.
        if source.is_some()
            && kind.is_some()
            && raw.is_none()
            && selections
                .iter()
                .all(|(_, reason)| reason.as_deref() == Some("context_replacement"))
        {
            continue;
        }
        let mut payload: Value = raw
            .as_deref()
            .and_then(|r| serde_json::from_str(r).ok())
            .unwrap_or(Value::Null);
        let known = payload.is_object()
            && (matches!(kind.as_deref(), Some("user") | Some("attachment"))
                || (kind.as_deref() == Some("event")
                    && payload.get("role").and_then(Value::as_str) == Some("user")
                    && payload.get("content").and_then(Value::as_array).is_some()));
        let source = if known {
            source.expect("joined source")
        } else {
            let previous_source = source;
            let source = format!("legacy-attachment-{attachment}");
            let payload=json!({"role":"user","content":[{"type":"input_text","text":"[Shellboy legacy attachment recovery] Original provenance or historical position could not be recovered. This auxiliary attachment source is not a new human message."},{"type":"attachment_ref","attachment_id":attachment}],"attachment_operation":{"origin":"legacy_unknown","previous_source_node_id":previous_source,"provenance_uncertain":true}}).to_string();
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'attachment',?2,?3,?4,?5)",params![source,chrono::Utc::now().to_rfc3339(),payload,payload.len() as i64,Sha256::digest(payload.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>()])?;
            tx.execute(
                "UPDATE attachment_ref SET source_node_id=?2 WHERE id=?1",
                params![attachment, source],
            )?;
            // Forks share one immutable source but adopt it independently.
            for (thread, reason) in &selections {
                let status = if reason.as_deref() == Some("context_replacement") {
                    "cancelled"
                } else {
                    "accepted"
                };
                tx.execute("INSERT INTO attachment_delivery(node_id,thread_id,status,operation_id,origin_json,created_at)VALUES(?1,?2,?3,?4,?5,?6)",params![source,thread,status,format!("migration:{attachment}"),json!({"origin":"legacy_unknown","provenance_uncertain":true}).to_string(),chrono::Utc::now().to_rfc3339()])?;
            }
            continue;
        };
        if let Some(text) = payload.get("content").and_then(Value::as_str) {
            payload["content"] = json!([{"type":"input_text","text":text}]);
        }
        if payload.get("content").and_then(Value::as_array).is_none() {
            payload["content"] = json!([]);
        }
        let content = payload["content"]
            .as_array_mut()
            .expect("normalized content");
        if !content.iter().any(|v| {
            v.get("type").and_then(Value::as_str) == Some("attachment_ref")
                && v.get("attachment_id").and_then(Value::as_str) == Some(attachment.as_str())
        }) {
            content.push(json!({"type":"attachment_ref","attachment_id":attachment}));
        }
        let payload = payload.to_string();
        tx.execute("UPDATE node SET payload_json=?2,payload_bytes=?3,body_hash=?4,payload_revision=payload_revision+1 WHERE id=?1",params![source,payload,payload.len() as i64,Sha256::digest(payload.as_bytes()).iter().map(|b|format!("{b:02x}")).collect::<String>()])?;
    }
    Ok(())
}

fn validate_schema(connection: &Connection) -> Result<i64, StorageError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(StorageError::Schema(format!(
            "user_version {version}; supported {SCHEMA_VERSION}"
        )));
    }
    let objects: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if version == 0 {
        if objects != 0 {
            return Err(StorageError::Schema(
                "unversioned nonempty database; automatic adoption is prohibited".into(),
            ));
        }
        return Ok(0);
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
    Ok(version)
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
