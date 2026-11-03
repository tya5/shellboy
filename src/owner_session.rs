//! Owner bootstrap and safe cancellation using existing async/DB/file workers.
use crate::{
    app::{DataOwner, StartupStage},
    control::Control,
    ipc::{Invocation, LocalServer},
    protocol::{ApiError, ErrorCode},
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};
use tokio::sync::{mpsc as async_mpsc, watch};
use tokio_util::sync::CancellationToken;

pub struct Prepared {
    pub owner: DataOwner,
    pub control: Arc<Control>,
    pub server: LocalServer,
    pub requests: async_mpsc::Receiver<Invocation>,
    pub cwd: PathBuf,
}
impl Prepared {
    pub async fn shutdown(self) -> Result<(), ApiError> {
        close(self.owner, Some(self.control), Some(self.server)).await
    }
}
pub(crate) async fn close(
    owner: DataOwner,
    control: Option<Arc<Control>>,
    server: Option<LocalServer>,
) -> Result<(), ApiError> {
    // Freeze external transport first; drain Core before closing its writer.
    let transport = match server {
        Some(server) => server.shutdown().await,
        None => Ok(()),
    };
    let core = match control {
        Some(control) => control.shutdown().await,
        None => Ok(()),
    };
    let database = owner.shutdown().await;
    transport.and(core).and(database)
}

pub async fn initialize(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    cancel: CancellationToken,
    progress: watch::Sender<StartupStage>,
) -> Result<Option<Prepared>, ApiError> {
    initialize_reporting(config, data, cancel, move |stage| {
        progress.send_replace(stage);
    })
    .await
}

/// TUI-only locale metadata shares the existing coalescing stage watch.
pub async fn initialize_localized(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    cancel: CancellationToken,
    progress: watch::Sender<crate::app::StartupProgress>,
) -> Result<Option<Prepared>, ApiError> {
    let migration_progress = Some(progress.clone());
    initialize_localized_reporting(
        config,
        data,
        cancel,
        migration_progress,
        move |stage, language, motion| {
            progress.send_replace(crate::app::StartupProgress {
                stage,
                language,
                motion,
                migration: None,
            });
        },
    )
    .await
}

// Keep reporting observational. Tests can cancel synchronously at a stage
// without treating a coalescing watch notification as DB-worker admission.
pub async fn initialize_reporting(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    cancel: CancellationToken,
    progress: impl Fn(StartupStage) + Send + Sync,
) -> Result<Option<Prepared>, ApiError> {
    initialize_localized_reporting(config, data, cancel, None, |stage, _, _| progress(stage)).await
}
async fn initialize_localized_reporting(
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    cancel: CancellationToken,
    migration_progress: Option<watch::Sender<crate::app::StartupProgress>>,
    progress: impl Fn(StartupStage, crate::config::UiLanguage, crate::config::MotionMode) + Send + Sync,
) -> Result<Option<Prepared>, ApiError> {
    let cwd = tokio::task::spawn_blocking(std::env::current_dir)
        .await
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Startup path worker failed"))?
        .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Cannot determine startup cwd"))?;
    if cancel.is_cancelled() {
        return Ok(None);
    }
    let owner = DataOwner::start_with_localized_progress(
        config,
        data,
        cwd.clone(),
        migration_progress,
        &progress,
    )
    .await?;
    let language = owner.loaded.config.ui.language;
    let motion = owner.loaded.config.ui.motion;
    if cancel.is_cancelled() {
        progress(StartupStage::Closing, language, motion);
        owner.shutdown().await?;
        return Ok(None);
    }
    let control_started = std::time::Instant::now();
    progress(StartupStage::Control, language, motion);
    let control = Control::new(&owner, cwd.clone());
    if let Err(error) = control.start().await {
        progress(StartupStage::Closing, language, motion);
        let _ = close(owner, Some(control), None).await;
        return Err(error);
    }
    owner
        .diagnostics
        .record_startup_stage(StartupStage::Control, control_started.elapsed(), None);
    if cancel.is_cancelled() {
        progress(StartupStage::Closing, language, motion);
        close(owner, Some(control), None).await?;
        return Ok(None);
    }
    let endpoint_started = std::time::Instant::now();
    progress(StartupStage::Endpoint, language, motion);
    let (send, requests) = async_mpsc::channel(128);
    let server = match LocalServer::bind(&owner, send).await {
        Ok(server) => server,
        Err(error) => {
            progress(StartupStage::Closing, language, motion);
            let _ = close(owner, Some(control), None).await;
            return Err(error);
        }
    };
    owner.diagnostics.record_startup_stage(
        StartupStage::Endpoint,
        endpoint_started.elapsed(),
        None,
    );
    let prepared = Prepared {
        owner,
        control,
        server,
        requests,
        cwd,
    };
    if cancel.is_cancelled() {
        progress(StartupStage::Closing, language, motion);
        prepared.shutdown().await?;
        return Ok(None);
    }
    Ok(Some(prepared))
}

/// One result only. If the terminal disappears, retain cleanup ownership instead
/// of dropping a live owner through an abandoned UI channel.
pub async fn deliver(
    result: Result<Option<Prepared>, ApiError>,
    send: mpsc::SyncSender<Result<Option<Prepared>, ApiError>>,
) {
    if let Err(mpsc::TrySendError::Full(result) | mpsc::TrySendError::Disconnected(result)) =
        send.try_send(result)
        && let Ok(Some(prepared)) = result
    {
        let _ = prepared.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::OwnerLock, storage::DbWorker};
    async fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root.path().join("data");
        let owner = DataOwner::start(Some(config.clone()), Some(data.clone()), root.path().into())
            .await
            .unwrap();
        owner.shutdown().await.unwrap();
        (root, config, data)
    }

    #[tokio::test]
    async fn localized_progress_keeps_loaded_language_through_cancel_and_watch_coalescing() {
        use crate::{app::StartupProgress, config::UiLanguage};
        for language in [UiLanguage::En, UiLanguage::Ja] {
            let (_root, config, data) = fixture().await;
            std::fs::write(
                &config,
                format!(
                    "[ui]\nlanguage = \"{}\"\n",
                    if language == UiLanguage::Ja {
                        "ja"
                    } else {
                        "en"
                    }
                ),
            )
            .unwrap();
            let token = CancellationToken::new();
            let cancel = token.clone();
            let (send, watch) = watch::channel(StartupProgress::default());
            let events = std::sync::Mutex::new(Vec::new());
            let result = initialize_localized_reporting(
                Some(config),
                Some(data.clone()),
                token,
                None,
                |stage, observed, motion| {
                    events.lock().unwrap().push((stage, observed));
                    send.send_replace(StartupProgress {
                        stage,
                        language: observed,
                        motion,
                        migration: None,
                    });
                    if stage == StartupStage::Database {
                        cancel.cancel();
                    }
                },
            )
            .await
            .unwrap();
            assert!(result.is_none());
            let events = events.into_inner().unwrap();
            assert_eq!(events[0], (StartupStage::Configuration, UiLanguage::En));
            assert!(
                events
                    .iter()
                    .skip(1)
                    .all(|(_, observed)| *observed == language)
            );
            assert_eq!(
                *watch.borrow(),
                StartupProgress {
                    stage: StartupStage::Closing,
                    language,
                    motion: crate::config::MotionMode::On,
                    migration: None,
                }
            );
            assert!(OwnerLock::acquire(&data).is_ok());
            assert!(!data.join("endpoint.json").exists());
        }
    }
    #[tokio::test]
    async fn localized_progress_stays_english_if_configuration_cannot_be_loaded() {
        use crate::{app::StartupProgress, config::UiLanguage};
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("invalid.toml");
        std::fs::write(&config, "[ui]\nlanguage = \"not-a-language\"\n").unwrap();
        let (send, watch) = watch::channel(StartupProgress::default());
        let result = initialize_localized(
            Some(config),
            Some(root.path().join("data")),
            CancellationToken::new(),
            send,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            *watch.borrow(),
            StartupProgress {
                stage: StartupStage::Configuration,
                language: UiLanguage::En,
                motion: crate::config::MotionMode::On,
                migration: None,
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_at_database_preparation_finishes_migration_before_releasing_owner() {
        let (_root, config, data) = fixture().await;
        let path = data.join("shellboy.db");
        tokio::task::spawn_blocking(move || {
            let db = rusqlite::Connection::open(path).unwrap();
            // Prepare an older supported fixture before startup. No second
            // writer remains to race the production 2-second busy timeout.
            db.execute_batch("BEGIN IMMEDIATE; DROP INDEX thread_parent; DROP INDEX wait_run_state; DROP INDEX wait_recovery_thread; DROP INDEX thread_answer_latest; DROP INDEX receipt_thread_order; DROP INDEX shell_started_readback; DROP INDEX shell_owner_started_readback; DROP INDEX compact_operation_thread; DROP INDEX automatic_compaction_turn; DROP INDEX node_turn; DROP INDEX IF EXISTS receipt_turn_source; DROP INDEX shell_active_owner; DROP INDEX run_thread_created; DROP INDEX shell_terminal_time; DROP INDEX draft_state_thread_updated; DROP INDEX draft_submitted_source_revision; ALTER TABLE turn DROP COLUMN interrupt_requested_at; ALTER TABLE receipt DROP COLUMN path_admission; DROP INDEX IF EXISTS node_event_retention; DROP INDEX IF EXISTS delivery_event_state; DROP INDEX IF EXISTS receipt_run_completion; DROP INDEX IF EXISTS attempts_completed_scope; DELETE FROM schema_migration WHERE version>=8; PRAGMA user_version=7; COMMIT;").unwrap();
        }).await.unwrap();
        let token = CancellationToken::new();
        let cancel = token.clone();
        let initializing_data = data.clone();
        let (send, progress) = watch::channel(StartupStage::Configuration);
        let result = crate::test_watchdog::watchdog(
            "cancelled startup finishes migration before releasing owner",
            initialize_reporting(
                Some(config),
                Some(data.clone()),
                token.clone(),
                move |stage| {
                    send.send_replace(stage);
                    if stage == StartupStage::Database {
                        assert!(
                            OwnerLock::acquire(&initializing_data).is_err(),
                            "owner is held when database preparation starts"
                        );
                        cancel.cancel();
                    }
                },
            ),
        )
        .await
        .expect("cancelled startup did not close")
        .unwrap();
        assert!(
            token.is_cancelled(),
            "the database preparation stage was reached"
        );
        assert!(result.is_none());
        assert_eq!(*progress.borrow(), StartupStage::Closing);
        assert!(
            OwnerLock::acquire(&data).is_ok(),
            "cancel result follows actual writer close"
        );
        assert!(
            !data.join("endpoint.json").exists(),
            "cancel before handoff publishes no endpoint"
        );
        let path = data.join("shellboy.db");
        let version = tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap()
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(
            version,
            crate::storage::SCHEMA_VERSION,
            "cancel does not pretend completed migration was rolled back"
        );
        // Actual admitted-writer lifetime is separately exercised without
        // SQLite contention by storage::owner_guard_outlives_all_callers_until_actual_writer_close.
    }
    #[tokio::test]
    async fn abandoned_ready_delivery_closes_owner_and_endpoint_without_core_admission() {
        let (_root, config, data) = fixture().await;
        let (send, _) = watch::channel(StartupStage::Configuration);
        let prepared = initialize(
            Some(config),
            Some(data.clone()),
            CancellationToken::new(),
            send,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(OwnerLock::acquire(&data).is_err());
        prepared
            .owner
            .db
            .call(|db| {
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        drop(receive);
        deliver(Ok(Some(prepared)), send).await;
        assert!(OwnerLock::acquire(&data).is_ok());
        assert!(!data.join("endpoint.json").exists());
    }
    #[tokio::test]
    async fn failed_recovery_rolls_back_and_closes_writer_before_failure_delivery() {
        for language in [crate::config::UiLanguage::En, crate::config::UiLanguage::Ja] {
            let (_root, config, data) = fixture().await;
            std::fs::write(
                &config,
                format!(
                    "[ui]\nlanguage = \"{}\"\n",
                    if language == crate::config::UiLanguage::Ja {
                        "ja"
                    } else {
                        "en"
                    }
                ),
            )
            .unwrap();
            let db = DbWorker::open(data.join("shellboy.db")).await.unwrap();
            db.call(|db| {
            db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','/AGENTS.md','fixture','fixture','now');
                INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','live','now');
                INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','running','now');
                CREATE TRIGGER fail_recovery BEFORE UPDATE ON turn BEGIN SELECT RAISE(ABORT,'fixture recovery rollback'); END;")?;
            Ok(())
        }).await.unwrap();
            db.shutdown().await.unwrap();
            let (send, progress) = watch::channel(crate::app::StartupProgress::default());
            let result = initialize_localized(
                Some(config),
                Some(data.clone()),
                CancellationToken::new(),
                send,
            )
            .await;
            assert!(result.is_err());
            assert_eq!(
                *progress.borrow(),
                crate::app::StartupProgress {
                    stage: StartupStage::Closing,
                    language,
                    motion: crate::config::MotionMode::On,
                    migration: None,
                }
            );
            let lock = OwnerLock::acquire(&data).unwrap();
            let db = DbWorker::open(data.join("shellboy.db")).await.unwrap();
            db.call(|db| {
                assert_eq!(
                    db.query_row("SELECT state FROM turn WHERE id='u'", [], |r| r
                        .get::<_, String>(0))?,
                    "running"
                );
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM node", [], |r| r.get::<_, i64>(0))?,
                    0,
                    "interruption snapshot was rolled back too"
                );
                Ok(())
            })
            .await
            .unwrap();
            db.shutdown().await.unwrap();
            drop(lock);
        }
    }
}
