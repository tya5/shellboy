//! Data-directory ownership and asynchronous startup/shutdown.
pub mod startup_stage;

use crate::{
    config::{Config, LoadedConfig, PathEnvironment},
    protocol::{ApiError, ErrorCode, PROTOCOL_VERSION},
    storage::{DbWorker, StorageError},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

pub fn storage_error(error: StorageError) -> ApiError {
    error.into_api_error()
}
fn io_error(error: std::io::Error) -> ApiError {
    ApiError::new(ErrorCode::IoFailed, error.to_string())
}

/// Shared with the DB OS thread. Its destructor unlocks only when both owner and
/// actual writer have released it; cancelling an observer does not release early.
pub struct OwnerLock {
    _file: File,
    #[cfg(windows)]
    directory: PathBuf,
}
impl Drop for OwnerLock {
    fn drop(&mut self) {
        // Closing only our descriptor can leave flock held by a fork/dup of
        // the same open-file description. The last legitimate owner releases
        // exclusion explicitly, after the DB writer has dropped its Arc too.
        let _ = FileExt::unlock(&self._file);
    }
}
impl OwnerLock {
    fn begin_session(&self) -> Result<bool, ApiError> {
        use std::io::{Read, Seek, SeekFrom, Write};
        let mut file = &self._file;
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let mut previous = Vec::new();
        file.take(128)
            .read_to_end(&mut previous)
            .map_err(io_error)?;
        let clean = previous == b"shellboy-owner-clean-v1\n";
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        file.set_len(0).map_err(io_error)?;
        file.write_all(b"shellboy-owner-active-v1\n")
            .map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        Ok(clean)
    }

    fn finish_session(&self) -> Result<(), ApiError> {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = &self._file;
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        file.set_len(0).map_err(io_error)?;
        file.write_all(b"shellboy-owner-clean-v1\n")
            .map_err(io_error)?;
        file.sync_all().map_err(io_error)
    }

    /// Blocking file I/O: call from startup's file worker, never the UI thread.
    pub fn acquire(data_dir: &Path) -> Result<Arc<Self>, ApiError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(data_dir)
                .map_err(io_error)?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(data_dir.components().collect::<PathBuf>())
                .map_err(|error| {
                    ApiError::new(
                        ErrorCode::IoFailed,
                        "The data directory cannot be opened safely. The final path component must be a real directory you own, not a symlink.",
                    )
                    .hint("Use --data-dir to specify a real directory you own, and check its read and write permissions.")
                    .details(serde_json::json!({"component":"data_directory", "reason":"directory_open_failed", "os_error":error.raw_os_error(), "cause":error.to_string()}))
                })?;
            let metadata = directory.metadata().map_err(io_error)?;
            // Never chmod through a symlink or alter another user's directory.
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(ApiError::new(
                    ErrorCode::IoFailed,
                    "The data directory cannot be secured for private use. Use --data-dir to specify a real directory you own.",
                ));
            }
            if metadata.mode() & 0o7777 != 0o700 {
                directory
                    .set_permissions(std::fs::Permissions::from_mode(0o700))
                    .map_err(io_error)?;
            }
        }
        #[cfg(windows)]
        let resolved = windows_data_dir(data_dir)?;
        #[cfg(windows)]
        let data_dir = resolved.as_path();
        #[cfg(not(any(unix, windows)))]
        std::fs::create_dir_all(data_dir).map_err(io_error)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data_dir.join("owner.lock"))
            .map_err(io_error)?;
        file.try_lock_exclusive().map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "This data directory already has an owner, or its lock is unavailable",
            )
            .hint("Connect to the running instance or use another --data-dir")
        })?;
        // Establish RAII ownership immediately after acquiring exclusion, so
        // a subsequent directory-setup failure also explicitly unlocks it.
        let owner = Arc::new(Self {
            _file: file,
            #[cfg(windows)]
            directory: resolved.clone(),
        });
        for relative in ["managed/clipboard", "tmp", "logs"] {
            std::fs::create_dir_all(data_dir.join(relative)).map_err(io_error)?;
        }
        Ok(owner)
    }
}

#[cfg(windows)]
fn windows_data_dir(path: &Path) -> Result<PathBuf, ApiError> {
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

    fn check_local(path: &Path) -> Result<(), ApiError> {
        let remote = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => true,
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                    let root = [u16::from(drive), b':' as u16, b'\\' as u16, 0];
                    // DRIVE_REMOTE (Win32): includes paths using mapped drive letters.
                    unsafe { GetDriveTypeW(root.as_ptr()) == 4 }
                }
                _ => false,
            },
            _ => false,
        };
        if remote {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "SQLite WAL requires a local disk; a network data directory is not supported",
            )
            .hint("Choose a local folder with --data-dir")
            .details(serde_json::json!({"reason":"network_data_directory"})));
        }
        Ok(())
    }

    // Check the existing ancestor too, before creating a directory behind a redirect.
    check_local(path)?;
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink()) {
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "The final data directory component must not be a symlink",
        ));
    }
    for ancestor in path.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(resolved) => {
                check_local(&resolved)?;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(error)),
        }
    }
    std::fs::create_dir_all(path).map_err(io_error)?;
    if std::fs::symlink_metadata(path)
        .map_err(io_error)?
        .file_type()
        .is_symlink()
    {
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "The final data directory component must not be a symlink",
        ));
    }
    let resolved = std::fs::canonicalize(path).map_err(io_error)?;
    check_local(&resolved)?;
    Ok(resolved)
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Endpoint {
    pub protocol_version: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::ipc::decode_owner_version"
    )]
    #[schemars(length(max = 128))]
    pub owner_version: Option<String>,
    pub instance_id: String,
    pub transport: String,
    pub address: String,
}

/// Initialization observations, never saved as database state or events.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupStage {
    Configuration,
    OwnerLock,
    Database,
    DatabasePragmas,
    DatabaseMigration,
    DatabaseValidate,
    DatabaseForeignKeys,
    DatabaseQuickCheck,
    InterruptedWork,
    Control,
    Endpoint,
    Closing,
}

impl StartupStage {
    pub(crate) fn failure(self, mut error: ApiError) -> ApiError {
        let hint = match self {
            Self::Configuration => "Check the selected configuration with shellboy config check.",
            Self::OwnerLock => "Check whether this data directory already has a running owner.",
            Self::Database | Self::DatabasePragmas => {
                "Check the selected data directory's storage and permissions."
            }
            Self::DatabaseMigration | Self::DatabaseValidate => {
                "Check that the selected Shellboy version supports this data directory's database schema."
            }
            Self::DatabaseForeignKeys | Self::DatabaseQuickCheck => {
                "Preserve the database and inspect it with shellboy storage check."
            }
            Self::InterruptedWork | Self::Control | Self::Closing => {
                "Read the owner log for the selected data directory."
            }
            Self::Endpoint => {
                "Check the selected data directory's local IPC endpoint and permissions."
            }
        };
        error.hint.get_or_insert_with(|| hint.to_owned());
        let details = error.details.get_or_insert_with(|| serde_json::json!({}));
        if let Some(fields) = details.as_object_mut() {
            fields
                .entry("startup_stage")
                .or_insert_with(|| serde_json::json!(self));
        }
        error
    }
}

#[test]
fn startup_failure_reports_stage_and_one_action() {
    use StartupStage::*;
    for (stage, key, action) in [
        (Configuration, "configuration", "shellboy config check"),
        (OwnerLock, "owner_lock", "running owner"),
        (Database, "database", "storage and permissions"),
        (
            DatabasePragmas,
            "database_pragmas",
            "storage and permissions",
        ),
        (
            DatabaseMigration,
            "database_migration",
            "supports this data directory's database schema",
        ),
        (
            DatabaseValidate,
            "database_validate",
            "supports this data directory's database schema",
        ),
        (
            DatabaseForeignKeys,
            "database_foreign_keys",
            "shellboy storage check",
        ),
        (
            DatabaseQuickCheck,
            "database_quick_check",
            "shellboy storage check",
        ),
        (InterruptedWork, "interrupted_work", "owner log"),
        (Control, "control", "owner log"),
        (Endpoint, "endpoint", "local IPC endpoint"),
        (Closing, "closing", "owner log"),
    ] {
        let error = stage.failure(
            ApiError::new(ErrorCode::IoFailed, "evidence")
                .details(serde_json::json!({"fact": true})),
        );
        assert_eq!(error.message, "evidence");
        let details = error.details.unwrap();
        assert_eq!(details["fact"], true);
        assert_eq!(details["startup_stage"], key);
        assert!(error.hint.unwrap().contains(action), "{key}");
    }
    let explicit =
        Endpoint.failure(ApiError::new(ErrorCode::IoFailed, "evidence").hint("specific action"));
    assert_eq!(explicit.hint.as_deref(), Some("specific action"));
}

/// One existing startup watch carries both facts so coalescing cannot lose locale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupProgress {
    pub stage: StartupStage,
    pub language: crate::config::UiLanguage,
    pub motion: crate::config::MotionMode,
    pub migration: Option<(u32, u32)>,
}
impl Default for StartupProgress {
    fn default() -> Self {
        Self {
            stage: StartupStage::Configuration,
            language: crate::config::UiLanguage::En,
            motion: crate::config::MotionMode::On,
            migration: None,
        }
    }
}
pub struct DataOwner {
    pub instance_id: String,
    pub loaded: LoadedConfig,
    pub db: DbWorker,
    pub diagnostics: crate::diagnostics::Diagnostics,
    pub(crate) endpoint: Arc<RwLock<Option<Endpoint>>>,
    pub(crate) ipc_refusals: Arc<crate::ipc::AdmissionRefusals>,
    _lock: Arc<OwnerLock>,
}
#[cfg(unix)]
const OWNER_FD_FLOOR: libc::rlim_t = 1024;

#[cfg(unix)]
fn owner_fd_soft_target(soft: libc::rlim_t, hard: libc::rlim_t) -> libc::rlim_t {
    soft.max(hard.min(OWNER_FD_FLOOR))
}

#[cfg(unix)]
fn ensure_owner_fd_floor() -> Option<libc::rlimit> {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limits is initialized, writable, and valid for both calls.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) != 0 {
            return None;
        }
        let target = owner_fd_soft_target(limits.rlim_cur, limits.rlim_max);
        if target != limits.rlim_cur {
            limits.rlim_cur = target;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &limits);
        }
        (libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) == 0).then_some(limits)
    }
}

impl DataOwner {
    /// Keep owner exclusion alive during a background width-cache update.
    pub fn width_cache_guard(&self) -> Arc<OwnerLock> {
        self._lock.clone()
    }
    pub async fn start(
        config_path: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        cwd: PathBuf,
    ) -> Result<Self, ApiError> {
        Self::start_with_progress(config_path, data_dir, cwd, |_| {}).await
    }
    pub async fn start_with_progress(
        config_path: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        cwd: PathBuf,
        progress: impl Fn(StartupStage) + Send,
    ) -> Result<Self, ApiError> {
        Self::start_with_localized_progress(config_path, data_dir, cwd, None, move |stage, _, _| {
            progress(stage)
        })
        .await
    }
    pub(crate) async fn start_with_localized_progress(
        config_path: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        cwd: PathBuf,
        migration_progress: Option<tokio::sync::watch::Sender<StartupProgress>>,
        progress: impl Fn(StartupStage, crate::config::UiLanguage, crate::config::MotionMode) + Send,
    ) -> Result<Self, ApiError> {
        let configuration_started = std::time::Instant::now();
        progress(
            StartupStage::Configuration,
            crate::config::UiLanguage::En,
            crate::config::MotionMode::On,
        );
        let loaded = tokio::task::spawn_blocking(move || {
            let env = PathEnvironment::current();
            let selected = crate::config_setup::selected_path(config_path.as_deref(), &cwd, &env)?;
            Config::load_with_overrides(config_path.as_deref(), data_dir.as_deref(), &cwd, &env)
                .map_err(|error| {
                    ApiError::new(ErrorCode::ConfigInvalid, error.to_string())
                        .hint(crate::config_setup::NEXT)
                        .details(serde_json::json!({"config_path":selected}))
                })
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Configuration worker failed"))
        .and_then(|result| result)
        .map_err(|error| StartupStage::Configuration.failure(error))?;
        let language = loaded.config.ui.language;
        let motion = loaded.config.ui.motion;
        let configuration_elapsed = configuration_started.elapsed();
        let lock_started = std::time::Instant::now();
        progress(StartupStage::OwnerLock, language, motion);
        let directory = loaded.data_dir.clone();
        let lock = tokio::task::spawn_blocking(move || OwnerLock::acquire(&directory))
            .await
            .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Owner lock worker failed"))
            .and_then(|result| result)
            .map_err(|error| StartupStage::OwnerLock.failure(error))?;
        // Keep database and endpoint I/O on the same resolved directory as the lock.
        #[cfg(windows)]
        let loaded = LoadedConfig {
            data_dir: lock.directory.clone(),
            ..loaded
        };
        let marker_lock = lock.clone();
        let directory = loaded.data_dir.clone();
        let clean = tokio::task::spawn_blocking(move || {
            let clean = marker_lock.begin_session()?;
            startup_stage::publish(&directory, StartupStage::Database);
            Ok::<_, ApiError>(clean)
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Owner marker worker failed"))
        .and_then(|result| result)
        .map_err(|error| StartupStage::OwnerLock.failure(error))?;
        let diagnostics = crate::diagnostics::Diagnostics::start(
            loaded.data_dir.join("logs"),
            loaded.config.logging.clone(),
        );
        #[cfg(unix)]
        match ensure_owner_fd_floor() {
            Some(limits) if limits.rlim_cur >= OWNER_FD_FLOOR => {}
            limits => diagnostics.record_owner_fd_limit(limits),
        }
        diagnostics.record_startup_stage(StartupStage::Configuration, configuration_elapsed, None);
        diagnostics.record_startup_stage(StartupStage::OwnerLock, lock_started.elapsed(), None);
        progress(StartupStage::Database, language, motion);
        let db = DbWorker::open_owned_reporting(
            loaded.data_dir.join("shellboy.db"),
            lock.clone(),
            startup_stage::DbProgress {
                directory: loaded.data_dir.clone(),
                watch: migration_progress,
            },
            diagnostics.clone(),
            clean,
        )
        .await
        .map_err(|error| StartupStage::Database.failure(storage_error(error)))?;
        db.attach_diagnostics(diagnostics.clone());
        diagnostics.record(
            crate::diagnostics::DiagnosticEvent::Startup,
            Default::default(),
        );
        let now = chrono::Utc::now().to_rfc3339();
        let recovery_started = std::time::Instant::now();
        let directory = loaded.data_dir.clone();
        let stage_lock = lock.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _owner = stage_lock;
            startup_stage::publish(&directory, StartupStage::InterruptedWork);
        })
        .await;
        progress(StartupStage::InterruptedWork, language, motion);
        let preserve_usage_waits = loaded.config.recovery.auto_retry_usage_limit;
        let recovery = db.call(move |connection| {
            let tx=connection.transaction()?;
            let capture_failures = crate::control::interruption::capture_startup_interrupted_context(&tx,&now)?;
            let mut report = crate::control::state::interrupt_previous_work(&tx, &now, preserve_usage_waits)?;
            let capture_threads = crate::control::interruption::staged_capture_threads(&tx)?;
            for thread in capture_threads {
                crate::control::interruption::consume_staged_captures(&tx,&thread,&now)?;
            }
            let repair_failures = std::mem::take(&mut report.failures);
            report.failures = capture_failures;
            report.failures.extend(repair_failures.into_iter().take(32-report.failures.len()));
            let queued=tx.prepare(STARTUP_QUEUED_SELECT)?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            let cancelled=serde_json::json!({"kind":"compact","state":"failed","result_uncertain":true,"ended_at":now,"outcome":{"error":ApiError::new(ErrorCode::InvalidState,"Previous instance ended; compaction was cancelled without replay")}}).to_string();
            for body in queued{if let Ok(ids)=serde_json::from_str::<Vec<String>>(&body){for operation in ids{tx.execute("INSERT OR REPLACE INTO app_meta(key,value) VALUES(?1,?2)",rusqlite::params![crate::compaction::CompactionKeys::result(&operation),cancelled])?;}}}
            tx.execute(STARTUP_QUEUED_DELETE,[])?;
            tx.execute(STARTUP_WAIT_DELETE,[])?;
            tx.execute("DELETE FROM runtime_pin",[])?;
            tx.commit()?;
            Ok(report)
        }).await;
        diagnostics.record_startup_stage(
            StartupStage::InterruptedWork,
            recovery_started.elapsed(),
            None,
        );
        let report = match recovery {
            Ok(report) => report,
            Err(error) => {
                progress(StartupStage::Closing, language, motion);
                let _ = db.shutdown().await;
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(loaded.config.io.shutdown_grace_ms),
                    diagnostics.shutdown(),
                )
                .await;
                return Err(StartupStage::InterruptedWork.failure(storage_error(error)));
            }
        };
        for failure in report.failures {
            diagnostics.record_tool_repair_failure(failure);
        }
        Ok(Self {
            instance_id: uuid::Uuid::new_v4().to_string(),
            loaded,
            db,
            diagnostics,
            endpoint: Arc::new(RwLock::new(None)),
            ipc_refusals: Arc::default(),
            _lock: lock,
        })
    }

    pub(crate) async fn publish_startup_stage(&self, stage: StartupStage) {
        let directory = self.loaded.data_dir.clone();
        let lock = self._lock.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _owner = lock;
            startup_stage::publish(&directory, stage);
        })
        .await;
    }

    pub async fn publish_endpoint(&self, mut endpoint: Endpoint) -> Result<(), ApiError> {
        if endpoint.instance_id != self.instance_id || endpoint.protocol_version != PROTOCOL_VERSION
        {
            return Err(ApiError::new(
                ErrorCode::ProtocolMismatch,
                "Endpoint does not match this owner",
            ));
        }
        let dir = self.loaded.data_dir.clone();
        endpoint.owner_version = crate::ipc::current_owner_version();
        let published = endpoint.clone();
        tokio::task::spawn_blocking(move || {
            let temporary = dir.join(format!("endpoint-{}.tmp", endpoint.instance_id));
            let data = serde_json::to_vec(&endpoint)
                .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Cannot serialize endpoint"))?;
            use std::io::Write;
            let mut file =
                crate::atomic_file::AtomicFile::new(temporary, true).map_err(io_error)?;
            file.file().write_all(&data).map_err(io_error)?;
            file.publish(&dir.join("endpoint.json")).map_err(io_error)
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Endpoint worker failed"))??;
        *self.endpoint.write().map_err(|_| {
            ApiError::new(
                ErrorCode::IoFailed,
                "Endpoint publication state unavailable",
            )
        })? = Some(published);
        Ok(())
    }

    /// Control must stop/reap all owned executions before calling this method.
    pub async fn shutdown(self) -> Result<(), ApiError> {
        use crate::diagnostics::{ShutdownBoundary as Edge, ShutdownScope, ShutdownStage as Stage};
        let started = std::time::Instant::now();
        let trace = |stage, boundary| {
            self.diagnostics.record_shutdown(
                ShutdownScope::Owner,
                stage,
                boundary,
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            )
        };
        trace(Stage::Database, Edge::Begin);
        let database_result = self.db.shutdown().await.map_err(storage_error);
        trace(Stage::Database, Edge::End);
        trace(Stage::Endpoint, Edge::Begin);
        let path = self.loaded.data_dir.join("endpoint.json");
        let instance = self.instance_id.clone();
        let metadata_result = tokio::task::spawn_blocking(move || {
            let ours = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Endpoint>(&b).ok())
                .is_some_and(|e| e.instance_id == instance);
            if ours {
                std::fs::remove_file(path).map_err(io_error)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Shutdown metadata worker failed"));
        trace(Stage::Endpoint, Edge::End);
        // Keep the writer alive through DB/metadata cleanup, including errors.
        // It cannot record its own completed join after closing its sender.
        trace(Stage::Diagnostics, Edge::Begin);
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(self.loaded.config.io.shutdown_grace_ms),
            self.diagnostics.shutdown(),
        )
        .await;
        let metadata_result = metadata_result?;
        database_result.and(metadata_result)?;
        let lock = self._lock.clone();
        let directory = self.loaded.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            lock.finish_session()?;
            let _ = startup_stage::clear(&directory);
            Ok(())
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Owner marker worker failed"))?
    }
}

// Startup recovery prefix predicates as primary-key ranges (#219). The old
// `key LIKE 'prefix.%'` form cannot use the app_meta primary key (LIKE is
// case-insensitive and the column is BINARY), so it scanned every summary.part.* /
// index.* row (660 ms on a real database). These keys are only ever written by
// CompactionKeys::{pending,inflight} and `wait.pending.{operation}` as exact
// lowercase ASCII prefixes, so the byte range [`prefix.`, `prefix/`) ('/' is '.'+1)
// selects the same rows as LIKE; LIKE's `_`/`%` wildcards do not occur in the prefixes.
const STARTUP_QUEUED_SELECT: &str = "SELECT value FROM app_meta WHERE (key>='compact.pending.' AND key<'compact.pending/') OR (key>='compact.inflight.' AND key<'compact.inflight/')";
const STARTUP_QUEUED_DELETE: &str = "DELETE FROM app_meta WHERE (key>='compact.pending.' AND key<'compact.pending/') OR (key>='compact.inflight.' AND key<'compact.inflight/')";
const STARTUP_WAIT_DELETE: &str =
    "DELETE FROM app_meta WHERE key>='wait.pending.' AND key<'wait.pending/'";

#[cfg(test)]
mod startup_range_tests {
    #[cfg(unix)]
    #[test]
    fn owner_fd_floor_raises_only_within_the_hard_limit() {
        assert_eq!(super::owner_fd_soft_target(256, libc::RLIM_INFINITY), 1024);
        assert_eq!(super::owner_fd_soft_target(256, 512), 512);
        assert_eq!(super::owner_fd_soft_target(256, 256), 256);
        assert_eq!(super::owner_fd_soft_target(2048, libc::RLIM_INFINITY), 2048);
    }

    use super::*;

    const OLD_SELECT: &str = "SELECT key FROM app_meta WHERE key LIKE 'compact.pending.%' OR key LIKE 'compact.inflight.%' ORDER BY key";
    const NEW_SELECT: &str = "SELECT key FROM app_meta WHERE (key>='compact.pending.' AND key<'compact.pending/') OR (key>='compact.inflight.' AND key<'compact.inflight/') ORDER BY key";

    fn fixture() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE app_meta(key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL)",
        )
        .unwrap();
        let keys = [
            crate::compaction::CompactionKeys::new("thread:1").pending(),
            crate::compaction::CompactionKeys::new("thread:日本").pending(),
            crate::compaction::CompactionKeys::new("a%_b").inflight(),
            crate::compaction::CompactionKeys::new("thread:1").automatic_active(),
            crate::compaction::CompactionKeys::result("op"),
            "compact.pending".into(),
            "compact.pendingx.t".into(),
            "compact.pending/t".into(),
            "wait.pending.w1".into(),
            "wait.pending".into(),
            "summary.part.abc".into(),
            "index.preview.x".into(),
        ];
        for key in keys {
            db.execute("INSERT INTO app_meta VALUES(?1,'v')", [key])
                .unwrap();
        }
        db
    }

    fn keys(db: &rusqlite::Connection, sql: &str) -> Vec<String> {
        db.prepare(sql)
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn range_predicates_match_like_and_use_the_primary_key() {
        let db = fixture();
        let old = keys(&db, OLD_SELECT);
        assert_eq!(old.len(), 3);
        assert_eq!(keys(&db, NEW_SELECT), old);
        let old_wait = keys(
            &db,
            "SELECT key FROM app_meta WHERE key LIKE 'wait.pending.%' ORDER BY key",
        );
        let new_wait = keys(
            &db,
            "SELECT key FROM app_meta WHERE key>='wait.pending.' AND key<'wait.pending/' ORDER BY key",
        );
        assert_eq!(old_wait, new_wait);
        for sql in [
            STARTUP_QUEUED_SELECT,
            STARTUP_QUEUED_DELETE,
            STARTUP_WAIT_DELETE,
        ] {
            let plan: Vec<String> = db
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let joined = plan.join("\n");
            assert!(
                joined.contains("SEARCH") && !joined.contains("SCAN"),
                "{sql}: {joined}"
            );
            assert!(
                joined.contains("PRIMARY KEY") || joined.contains("sqlite_autoindex_app_meta_1"),
                "{joined}"
            );
        }
        let removed = db.execute(STARTUP_QUEUED_DELETE, []).unwrap();
        assert_eq!(removed, 3);
        assert_eq!(db.execute(STARTUP_WAIT_DELETE, []).unwrap(), 1);
    }
}

#[cfg(all(test, unix))]
mod owner_lock_tests {
    use super::*;

    #[test]
    fn data_directory_is_private_under_group_writable_umask() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        const CHILD: &str = "SHELLBOY_TEST_PRIVATE_DIRECTORY";
        if let Some(data) = std::env::var_os(CHILD) {
            // Isolated test subprocess: never mutate the parent test process's umask.
            unsafe { libc::umask(0o002) };
            let data = PathBuf::from(data);
            let lock = OwnerLock::acquire(&data).unwrap();
            assert_eq!(std::fs::metadata(&data).unwrap().mode() & 0o777, 0o700);
            drop(lock);
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o775)).unwrap();
            let reopened = OwnerLock::acquire(&data).unwrap();
            assert_eq!(std::fs::metadata(&data).unwrap().mode() & 0o777, 0o700);
            drop(reopened);
            let target = data.parent().unwrap().join("unrelated");
            std::fs::create_dir(&target).unwrap();
            let before = std::fs::metadata(&target).unwrap().mode();
            let alias = data.parent().unwrap().join("alias");
            std::os::unix::fs::symlink(&target, &alias).unwrap();
            let error = match OwnerLock::acquire(&PathBuf::from(format!("{}/", alias.display()))) {
                Ok(_) => panic!("symlink data directory must be refused"),
                Err(error) => error,
            };
            assert!(error.message.contains("symlink"));
            assert!(error.hint.as_deref().unwrap().contains("--data-dir"));
            assert_eq!(error.details.unwrap()["reason"], "directory_open_failed");
            assert_eq!(std::fs::metadata(target).unwrap().mode(), before);
            let config = data.parent().unwrap().join("config.toml");
            std::fs::write(&config, "").unwrap();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let owner = DataOwner::start(Some(config), Some(data.clone()), data.clone())
                        .await
                        .unwrap();
                    let (sender, _) = tokio::sync::mpsc::channel(1);
                    let server = crate::ipc::LocalServer::bind(&owner, sender).await.unwrap();
                    let endpoint: Endpoint =
                        serde_json::from_slice(&std::fs::read(data.join("endpoint.json")).unwrap())
                            .unwrap();
                    let folder = Path::new(&endpoint.address).parent().unwrap();
                    let metadata = std::fs::symlink_metadata(folder).unwrap();
                    let uid = unsafe { libc::geteuid() };
                    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
                    assert_eq!(metadata.uid(), uid);
                    assert_eq!(metadata.mode() & 0o7777, 0o700);
                    assert!(endpoint.address.len() <= 103);
                    let base = folder.parent().unwrap();
                    let fallback = std::fs::canonicalize("/tmp").unwrap();
                    #[cfg(target_os = "macos")]
                    let expected = {
                        use std::os::unix::ffi::OsStringExt;
                        // Ask the OS directly, independently of inherited TMPDIR.
                        let mut bytes = vec![0u8; 4096];
                        let length = unsafe {
                            libc::confstr(
                                libc::_CS_DARWIN_USER_TEMP_DIR,
                                bytes.as_mut_ptr().cast(),
                                bytes.len(),
                            )
                        };
                        let user = (length > 0 && length <= bytes.len()).then(|| {
                            bytes.truncate(length - 1);
                            PathBuf::from(std::ffi::OsString::from_vec(bytes))
                        });
                        user.and_then(|path| std::fs::canonicalize(path).ok())
                            .filter(|path| {
                                let socket = path.join(folder.file_name().unwrap()).join("s");
                                let private = std::fs::symlink_metadata(path).is_ok_and(|m| {
                                    m.is_dir() && m.uid() == uid && m.mode() & 0o7777 == 0o700
                                });
                                private
                                    && socket.to_str().is_some()
                                    && socket.as_os_str().len() <= 103
                                    && path.ancestors().all(|parent| {
                                        std::fs::symlink_metadata(parent).is_ok_and(|m| {
                                            m.is_dir()
                                                && !m.file_type().is_symlink()
                                                && (m.uid() == 0 || m.uid() == uid)
                                                && (m.mode() & 0o022 == 0 || m.mode() & 0o1000 != 0)
                                        })
                                    })
                            })
                            .unwrap_or(fallback)
                    };
                    #[cfg(not(target_os = "macos"))]
                    let expected = fallback;
                    assert_eq!(base, expected);
                    server.shutdown().await.unwrap();
                    owner.shutdown().await.unwrap();
                });
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::owner_lock_tests::data_directory_is_private_under_group_writable_umask",
                "--nocapture",
            ])
            .env(CHILD, root.path().join("data"))
            .env_remove("XDG_RUNTIME_DIR")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::metadata(root.path()).unwrap().mode() & 0o777,
            0o775
        );
    }

    #[tokio::test]
    async fn completed_owner_releases_lock_even_when_a_duplicated_descriptor_is_still_open() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root.path().join("data");
        let owner = DataOwner::start(Some(config), Some(data.clone()), root.path().into())
            .await
            .unwrap();
        // Like fork before exec, dup retains the same open-file description.
        // This models the ownership boundary, not proof of a particular fork.
        let inherited = owner._lock._file.try_clone().unwrap();
        let weak = Arc::downgrade(&owner._lock);
        owner.shutdown().await.unwrap();
        assert!(weak.upgrade().is_none(), "both legitimate owners finished");
        assert!(inherited.metadata().unwrap().is_file());
        let reopened = OwnerLock::acquire(&data);
        drop(inherited);
        assert!(
            reopened.is_ok(),
            "an unrelated retained descriptor must not outlive owner exclusion"
        );
        assert!(
            OwnerLock::acquire(&data).is_err(),
            "closing the old duplicate must not unlock the new owner"
        );
    }

    #[tokio::test]
    async fn live_database_retains_exclusion_until_its_real_writer_is_joined() {
        let root = tempfile::tempdir().unwrap();
        let lock = OwnerLock::acquire(root.path()).unwrap();
        let inherited = lock._file.try_clone().unwrap();
        let weak = Arc::downgrade(&lock);
        let database = DbWorker::open_owned(root.path().join("shellboy.db"), lock.clone())
            .await
            .unwrap();
        drop(lock);
        assert!(weak.upgrade().is_some());
        assert!(
            OwnerLock::acquire(root.path()).is_err(),
            "a live writer still owns its directory"
        );
        database.shutdown().await.unwrap();
        assert!(weak.upgrade().is_none());
        assert!(inherited.metadata().unwrap().is_file());
        let reopened = OwnerLock::acquire(root.path());
        drop(inherited);
        assert!(
            reopened.is_ok(),
            "writer join ends ownership despite an inherited descriptor"
        );
        assert!(
            OwnerLock::acquire(root.path()).is_err(),
            "new owner remains exclusive after the old duplicate closes"
        );
    }
}

#[cfg(test)]
mod integrity_marker_tests {
    use super::*;

    #[test]
    fn owner_marker_defaults_to_unclean_and_requires_explicit_finish() {
        let root = tempfile::tempdir().unwrap();
        let lock = OwnerLock::acquire(root.path()).unwrap();
        assert!(!lock.begin_session().unwrap());
        drop(lock);
        let lock = OwnerLock::acquire(root.path()).unwrap();
        assert!(!lock.begin_session().unwrap());
        lock.finish_session().unwrap();
        drop(lock);
        let lock = OwnerLock::acquire(root.path()).unwrap();
        assert!(lock.begin_session().unwrap());
        drop(lock);
        let lock = OwnerLock::acquire(root.path()).unwrap();
        assert!(!lock.begin_session().unwrap());
    }
}

#[cfg(all(test, windows))]
mod windows_owner_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[tokio::test]
    async fn owner_starts_in_locally_redirected_directory_and_replaces_stale_endpoint() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("local target");
        let redirect = root.path().join("redirect");
        std::fs::create_dir(&local).unwrap();
        let junction = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&redirect)
            .arg(&local)
            .output()
            .unwrap();
        assert!(junction.status.success(), "{junction:?}");
        let data = redirect.join("data");
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = DataOwner::start(Some(config), Some(data.clone()), root.path().to_owned())
            .await
            .unwrap();
        assert_eq!(
            owner.loaded.data_dir,
            local.join("data").canonicalize().unwrap()
        );
        assert!(local.join("data/shellboy.db").is_file());
        let target = data.join("endpoint.json");
        std::fs::write(&target, b"stale endpoint").unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        let endpoint = Endpoint {
            protocol_version: PROTOCOL_VERSION,
            owner_version: None,
            instance_id: owner.instance_id.clone(),
            transport: "test".into(),
            address: "test".into(),
        };
        assert!(owner.publish_endpoint(endpoint.clone()).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"stale endpoint");
        assert!(
            !data
                .join(format!("endpoint-{}.tmp", owner.instance_id))
                .exists()
        );
        drop(lock);
        owner.publish_endpoint(endpoint).await.unwrap();
        let actual: Endpoint = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(actual.instance_id, owner.instance_id);
        owner.shutdown().await.unwrap();
    }

    #[test]
    fn owner_refuses_network_data_directory_without_contacting_server() {
        for path in [
            r"\\uncontacted.invalid\share\data",
            r"\\?\UNC\uncontacted.invalid\share\data",
        ] {
            let Err(error) = OwnerLock::acquire(Path::new(path)) else {
                panic!("expected network refusal")
            };
            assert_eq!(error.details.unwrap()["reason"], "network_data_directory");
        }
    }
}
