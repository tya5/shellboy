//! Bounded nonblocking diagnostics: generated IDs and fixed model classifications only.
//! Model/provider text and request/response bodies never enter the writer queue.
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
    ModelStreamDifference,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<ModelFailureDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_difference: Option<ModelFailureDetails>,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelFailureReason {
    Preparation,
    Recovery,
    StreamDifference,
    StreamDeltaDifference,
    Protocol,
    Transport,
    Provider,
    Http,
    Incomplete,
    InputLimit,
    UnsupportedItem,
    Cancelled,
    ConsumerClosed,
    Configuration,
    Timeout,
    Unknown,
}
#[derive(Serialize)]
struct ModelFailureDetails {
    reason: ModelFailureReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<crate::model::OutputMismatchField>,
}
impl ModelFailureDetails {
    fn stage(reason: ModelFailureReason) -> Self {
        Self {
            reason,
            attempt_id: None,
            output_index: None,
            field: None,
        }
    }
    fn attempt(attempt_id: &str, error: &crate::model::ModelError) -> Self {
        // This is an exact allowlist, never a message/provider-code substring check.
        let reason = match error.kind.as_str() {
            "protocol" => ModelFailureReason::Protocol,
            "transport" => ModelFailureReason::Transport,
            "provider" => ModelFailureReason::Provider,
            "http" => ModelFailureReason::Http,
            "incomplete" => ModelFailureReason::Incomplete,
            "input_limit" => ModelFailureReason::InputLimit,
            "unsupported_item" => ModelFailureReason::UnsupportedItem,
            "cancelled" => ModelFailureReason::Cancelled,
            "consumer_closed" => ModelFailureReason::ConsumerClosed,
            "config_missing"
            | "config_missing_credentials"
            | "config_invalid"
            | "dimension_mismatch" => ModelFailureReason::Configuration,
            "timeout" => ModelFailureReason::Timeout,
            _ => ModelFailureReason::Unknown,
        };
        Self {
            reason,
            attempt_id: Some(sanitize_id(attempt_id)),
            output_index: None,
            field: None,
        }
    }
}
struct Record {
    event: DiagnosticEvent,
    ids: DiagnosticIds,
    failure: Option<ModelFailureDetails>,
    stream_difference: Option<ModelFailureDetails>,
}
struct Health {
    failed: AtomicBool,
    dropped: AtomicU64,
}
struct Shared {
    sender: Mutex<Option<SyncSender<Record>>>,
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
                while let Ok(Record {
                    event,
                    ids,
                    failure,
                    stream_difference,
                }) = receiver.recv()
                {
                    let dropped = worker_health.dropped.swap(0, Ordering::AcqRel);
                    let entry = Entry {
                        at: chrono::Utc::now().to_rfc3339(),
                        event,
                        ids,
                        dropped,
                        failure,
                        stream_difference,
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
    pub fn record(&self, event: DiagnosticEvent, ids: DiagnosticIds) {
        let failure = matches!(event, DiagnosticEvent::ModelFailure)
            .then(|| ModelFailureDetails::stage(ModelFailureReason::Recovery));
        self.enqueue(event, ids, failure, None);
    }
    /// Preserve fixed failure facts before model errors become public recovery errors.
    /// No error text, provider code, endpoint, or response body is queued.
    pub fn record_model_failure(
        &self,
        ids: DiagnosticIds,
        attempt_id: &str,
        error: &crate::model::ModelError,
    ) {
        self.enqueue(
            DiagnosticEvent::ModelFailure,
            ids,
            Some(ModelFailureDetails::attempt(attempt_id, error)),
            None,
        );
    }
    /// A difference is an observation, not a failed or rejected final response.
    /// Only the fixed field category is logged; neither received value is copied.
    pub fn record_model_stream_difference(
        &self,
        ids: DiagnosticIds,
        attempt_id: &str,
        difference: &crate::model::OutputMismatch,
    ) {
        self.enqueue(
            DiagnosticEvent::ModelStreamDifference,
            ids,
            None,
            Some(ModelFailureDetails {
                reason: ModelFailureReason::StreamDifference,
                attempt_id: Some(sanitize_id(attempt_id)),
                output_index: Some(difference.output_index),
                field: Some(difference.field),
            }),
        );
    }
    /// Response-wide delta columns have no authoritative output-item index.
    pub fn record_model_stream_delta_difference(
        &self,
        ids: DiagnosticIds,
        attempt_id: &str,
        field: crate::model::OutputMismatchField,
    ) {
        self.enqueue(
            DiagnosticEvent::ModelStreamDifference,
            ids,
            None,
            Some(ModelFailureDetails {
                reason: ModelFailureReason::StreamDeltaDifference,
                attempt_id: Some(sanitize_id(attempt_id)),
                output_index: None,
                field: Some(field),
            }),
        );
    }
    pub(crate) fn record_preparation_failure(&self, ids: DiagnosticIds) {
        self.enqueue(
            DiagnosticEvent::ModelFailure,
            ids,
            Some(ModelFailureDetails::stage(ModelFailureReason::Preparation)),
            None,
        );
    }
    fn enqueue(
        &self,
        event: DiagnosticEvent,
        mut ids: DiagnosticIds,
        failure: Option<ModelFailureDetails>,
        stream_difference: Option<ModelFailureDetails>,
    ) {
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
            *id = sanitize_id(id);
        }
        let Ok(sender) = self.shared.sender.try_lock() else {
            self.shared.health.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if sender.as_ref().is_none_or(|sender| {
            sender
                .try_send(Record {
                    event,
                    ids,
                    failure,
                    stream_difference,
                })
                .is_err()
        }) {
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

fn sanitize_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_:".contains(*c))
        .take(128)
        .collect()
}
