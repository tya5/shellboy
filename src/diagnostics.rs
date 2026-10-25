//! Bounded nonblocking diagnostics. Only event names and generated IDs are logged.
use crate::config::Logging;
use serde::Serialize;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticEvent {
    Startup,
    Shutdown,
    DatabaseFailure,
    ModelFailure,
    ShellFailure,
    StorageLimit,
    IndexPaused,
}
#[derive(Debug, Default, Clone, Serialize)]
pub struct DiagnosticIds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
}
#[derive(Serialize)]
struct Entry {
    at: String,
    event: DiagnosticEvent,
    ids: DiagnosticIds,
    dropped: u64,
}
struct Health {
    failed: AtomicBool,
    dropped: AtomicU64,
}
struct Shared {
    sender: Mutex<Option<SyncSender<(DiagnosticEvent, DiagnosticIds)>>>,
    join: Mutex<Option<JoinHandle<()>>>,
    health: Arc<Health>,
}
#[derive(Clone)]
pub struct Diagnostics {
    shared: Arc<Shared>,
}
impl Diagnostics {
    /// Starts a dedicated writer; no file open, metadata, or write on the caller.
    pub fn start(directory: PathBuf, settings: Logging) -> Self {
        let health = Arc::new(Health {
            failed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        });
        let (sender, receiver) = mpsc::sync_channel(256);
        let worker_health = health.clone();
        let join = std::thread::Builder::new()
            .name("shellboy-diagnostics".into())
            .spawn(move || {
                let mut writer: Option<File> = None;
                let mut bytes = 0u64;
                while let Ok((event, ids)) = receiver.recv() {
                    let dropped = worker_health.dropped.swap(0, Ordering::AcqRel);
                    let entry = Entry {
                        at: chrono::Utc::now().to_rfc3339(),
                        event,
                        ids,
                        dropped,
                    };
                    let mut line = match serde_json::to_vec(&entry) {
                        Ok(value) => value,
                        Err(_) => {
                            worker_health.failed.store(true, Ordering::Release);
                            continue;
                        }
                    };
                    line.push(b'\n');
                    if line.len() as u64 > settings.file_bytes {
                        worker_health.failed.store(true, Ordering::Release);
                        worker_health
                            .dropped
                            .fetch_add(1 + dropped, Ordering::Relaxed);
                        continue;
                    }
                    let result = (|| -> std::io::Result<()> {
                        if writer.is_none() {
                            std::fs::create_dir_all(&directory)?;
                            let file = OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(directory.join("shellboy.log"))?;
                            bytes = file.metadata()?.len();
                            writer = Some(file);
                        }
                        if bytes + line.len() as u64 > settings.file_bytes {
                            drop(writer.take());
                            rotate(&directory, settings.files)?;
                            writer = Some(
                                OpenOptions::new()
                                    .create(true)
                                    .truncate(true)
                                    .write(true)
                                    .open(directory.join("shellboy.log"))?,
                            );
                            bytes = 0;
                        }
                        writer.as_mut().expect("writer opened").write_all(&line)?;
                        bytes += line.len() as u64;
                        Ok(())
                    })();
                    if result.is_err() {
                        writer.take();
                        worker_health.failed.store(true, Ordering::Release);
                        worker_health
                            .dropped
                            .fetch_add(1 + dropped, Ordering::Relaxed);
                    }
                }
                if let Some(mut writer) = writer
                    && writer.flush().is_err()
                {
                    worker_health.failed.store(true, Ordering::Release);
                }
            })
            .ok();
        if join.is_none() {
            health.failed.store(true, Ordering::Release);
        }
        Self {
            shared: Arc::new(Shared {
                sender: Mutex::new(Some(sender)),
                join: Mutex::new(join),
                health,
            }),
        }
    }
    /// Finite try_send only. This method is safe to call on the UI main thread.
    pub fn record(&self, event: DiagnosticEvent, mut ids: DiagnosticIds) {
        // IDs come from owned records, but keep even accidental callers bounded.
        for id in [
            &mut ids.thread_id,
            &mut ids.run_id,
            &mut ids.turn_id,
            &mut ids.execution_id,
        ]
        .into_iter()
        .flatten()
        {
            *id = id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || "-_:".contains(*c))
                .take(128)
                .collect();
        }
        let Ok(sender) = self.shared.sender.try_lock() else {
            self.shared.health.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if sender
            .as_ref()
            .is_none_or(|sender| sender.try_send((event, ids)).is_err())
        {
            self.shared.health.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn has_failed(&self) -> bool {
        self.shared.health.failed.load(Ordering::Acquire)
    }
    pub fn pending_dropped(&self) -> u64 {
        self.shared.health.dropped.load(Ordering::Relaxed)
    }
    /// Call during orderly shutdown from an async task, with an outer deadline.
    pub async fn shutdown(&self) {
        self.shared
            .sender
            .lock()
            .expect("diagnostic sender poisoned")
            .take();
        let join = self
            .shared
            .join
            .lock()
            .expect("diagnostic join poisoned")
            .take();
        if let Some(join) = join {
            let health = self.shared.health.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if join.join().is_err() {
                    health.failed.store(true, Ordering::Release);
                }
            })
            .await;
        }
    }
}
fn rotate(directory: &Path, files: usize) -> std::io::Result<()> {
    if files <= 1 {
        match std::fs::remove_file(directory.join("shellboy.log")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        };
        return Ok(());
    }
    let oldest = directory.join(format!("shellboy.log.{}", files - 1));
    if oldest.exists() {
        std::fs::remove_file(oldest)?;
    }
    for index in (1..files - 1).rev() {
        let path = directory.join(format!("shellboy.log.{index}"));
        if path.exists() {
            std::fs::rename(path, directory.join(format!("shellboy.log.{}", index + 1)))?;
        }
    }
    let current = directory.join("shellboy.log");
    if current.exists() {
        std::fs::rename(current, directory.join("shellboy.log.1"))?;
    }
    Ok(())
}
