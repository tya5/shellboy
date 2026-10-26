//! Data-directory ownership and asynchronous startup/shutdown.
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
                .map_err(io_error)?;
            let metadata = directory.metadata().map_err(io_error)?;
            // Never chmod through a symlink or alter another user's directory.
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(ApiError::new(
                    ErrorCode::IoFailed,
                    "保存先を専用にできません。自分が所有する実ディレクトリを --data-dir で指定してください",
                ));
            }
            if metadata.mode() & 0o7777 != 0o700 {
                directory
                    .set_permissions(std::fs::Permissions::from_mode(0o700))
                    .map_err(io_error)?;
            }
        }
        #[cfg(not(unix))]
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
        let owner = Arc::new(Self { _file: file });
        for relative in ["managed/clipboard", "tmp", "logs"] {
            std::fs::create_dir_all(data_dir.join(relative)).map_err(io_error)?;
        }
        Ok(owner)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub protocol_version: u32,
    pub instance_id: String,
    pub transport: String,
    pub address: String,
}

/// Ephemeral initialization observations, never persisted Core state/events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupStage {
    Configuration,
    OwnerLock,
    Database,
    InterruptedWork,
    Control,
    Endpoint,
    Closing,
}

pub struct DataOwner {
    pub instance_id: String,
    pub loaded: LoadedConfig,
    pub db: DbWorker,
    pub diagnostics: crate::diagnostics::Diagnostics,
    pub(crate) endpoint: Arc<RwLock<Option<Endpoint>>>,
    _lock: Arc<OwnerLock>,
}
impl DataOwner {
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
        progress(StartupStage::Configuration);
        let loaded = tokio::task::spawn_blocking(move || {
            Config::load_with_overrides(
                config_path.as_deref(),
                data_dir.as_deref(),
                &cwd,
                &PathEnvironment::current(),
            )
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Configuration worker failed"))?
        .map_err(|e| ApiError::new(ErrorCode::ConfigInvalid, e.to_string()))?;
        progress(StartupStage::OwnerLock);
        let directory = loaded.data_dir.clone();
        let lock = tokio::task::spawn_blocking(move || OwnerLock::acquire(&directory))
            .await
            .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Owner lock worker failed"))??;
        progress(StartupStage::Database);
        let db = DbWorker::open_owned(loaded.data_dir.join("shellboy.db"), lock.clone())
            .await
            .map_err(storage_error)?;
        let diagnostics = crate::diagnostics::Diagnostics::start(
            loaded.data_dir.join("logs"),
            loaded.config.logging.clone(),
        );
        diagnostics.record(
            crate::diagnostics::DiagnosticEvent::Startup,
            Default::default(),
        );
        let now = chrono::Utc::now().to_rfc3339();
        progress(StartupStage::InterruptedWork);
        let recovery = db.call(move |connection| {
            let tx=connection.transaction()?;
            crate::control::interruption::capture_interrupted_context(
                &tx, &now, "previous_instance_interrupted", "startup",
            )?;
            crate::control::state::interrupt_previous_work(&tx, &now)?;
            let queued=tx.prepare("SELECT value FROM app_meta WHERE key LIKE 'compact.pending.%' OR key LIKE 'compact.inflight.%'")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            let cancelled=serde_json::json!({"kind":"compact","state":"failed","result_uncertain":true,"ended_at":now,"outcome":{"error":ApiError::new(ErrorCode::InvalidState,"Previous instance ended; compaction was cancelled without replay")}}).to_string();
            for body in queued{if let Ok(ids)=serde_json::from_str::<Vec<String>>(&body){for operation in ids{tx.execute("INSERT OR REPLACE INTO app_meta(key,value) VALUES(?1,?2)",rusqlite::params![crate::compaction::CompactionKeys::result(&operation),cancelled])?;}}}
            tx.execute("DELETE FROM app_meta WHERE key LIKE 'compact.pending.%' OR key LIKE 'compact.inflight.%'",[])?;
            tx.execute("DELETE FROM app_meta WHERE key LIKE 'wait.pending.%'",[])?;
            tx.execute("DELETE FROM runtime_pin",[])?;
            tx.commit()?;
            Ok(())
        }).await;
        if let Err(error) = recovery {
            progress(StartupStage::Closing);
            let _ = db.shutdown().await;
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(loaded.config.io.shutdown_grace_ms),
                diagnostics.shutdown(),
            )
            .await;
            return Err(storage_error(error));
        }
        Ok(Self {
            instance_id: uuid::Uuid::new_v4().to_string(),
            loaded,
            db,
            diagnostics,
            endpoint: Arc::new(RwLock::new(None)),
            _lock: lock,
        })
    }

    pub async fn publish_endpoint(&self, endpoint: Endpoint) -> Result<(), ApiError> {
        if endpoint.instance_id != self.instance_id || endpoint.protocol_version != PROTOCOL_VERSION
        {
            return Err(ApiError::new(
                ErrorCode::ProtocolMismatch,
                "Endpoint does not match this owner",
            ));
        }
        let dir = self.loaded.data_dir.clone();
        let published = endpoint.clone();
        tokio::task::spawn_blocking(move || {
            let temporary = dir.join(format!("endpoint-{}.tmp", endpoint.instance_id));
            let data = serde_json::to_vec(&endpoint)
                .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Cannot serialize endpoint"))?;
            use std::io::Write;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(io_error)?;
            file.write_all(&data)
                .and_then(|_| file.sync_all())
                .map_err(io_error)?;
            drop(file);
            // Windows rename does not replace existing files; owner lock prevents
            // another writer while replacing stale endpoint metadata.
            let target = dir.join("endpoint.json");
            #[cfg(windows)]
            if target.exists() {
                std::fs::remove_file(&target).map_err(io_error)?;
            }
            std::fs::rename(temporary, target).map_err(io_error)
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
        let database_result = self.db.shutdown().await.map_err(storage_error);
        self.diagnostics.record(
            crate::diagnostics::DiagnosticEvent::Shutdown,
            Default::default(),
        );
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(self.loaded.config.io.shutdown_grace_ms),
            self.diagnostics.shutdown(),
        )
        .await;
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
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Shutdown metadata worker failed"))?;
        database_result.and(metadata_result)
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
            assert!(OwnerLock::acquire(&PathBuf::from(format!("{}/", alias.display()))).is_err());
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
                    assert_eq!(
                        Path::new(&endpoint.address)
                            .parent()
                            .unwrap()
                            .parent()
                            .unwrap(),
                        std::fs::canonicalize("/tmp").unwrap()
                    );
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
