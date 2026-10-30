//! Bounded nonblocking diagnostics: fixed classifications and numeric observations only.
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
    MessageSend,
    ShellFailure,
    StorageLimit,
    IndexPaused,
    UsageTotalsSlow,
    UiHistory,
}

/// Fixed frontend observations; never accepts history or shell content.
#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiHistoryReason {
    #[default]
    WindowApplied,
    StaleWindow,
    UserFollow,
    BodyEviction,
}
/// Ephemeral presentation identities and counts on the existing bounded queue.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct UiHistoryTrace {
    pub reason: UiHistoryReason,
    pub generation: u64,
    pub view_revision: u64,
    pub read_revision: u64,
    pub before_nodes: u64,
    pub after_nodes: u64,
    pub evicted_bodies: u64,
    pub follow_before: bool,
    pub follow_after: bool,
}
pub(crate) const USAGE_SLOW_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(100);
#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageTrigger {
    Open,
    Changed,
    #[default]
    Cli,
}
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageScope {
    Turn,
    Run,
    Thread,
    ThreadChildren,
}
#[derive(Serialize)]
struct UsageSlowTrace {
    trigger: UsageTrigger,
    scope: UsageScope,
    elapsed_ms: u64,
}
fn slow_usage_trace(
    trigger: UsageTrigger,
    scope: UsageScope,
    elapsed: std::time::Duration,
) -> Option<UsageSlowTrace> {
    (elapsed >= USAGE_SLOW_THRESHOLD).then(|| UsageSlowTrace {
        trigger,
        scope,
        elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    })
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
/// Frontend milestones, not durable message/Run states.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSendStage {
    Queued,
    Processing,
    Saved,
    Failed,
    QueueFull,
}
#[derive(Debug, Clone, Serialize)]
pub struct MessageSendTrace {
    pub stage: MessageSendStage,
    pub draft_id: String,
    pub revision: u64,
    pub operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}
/// Fixed structural reasons only; no source/tool text enters diagnostics.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolRepairReason {
    UnknownGroupState,
    UnknownCallState,
    MissingCallSource,
    ExecutionTurnMismatch,
    AmbiguousCallSource,
    CursorMismatch,
    CaptureCursorMismatch,
    CaptureRevisitMalformed,
    OrdinalOverflow,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolRepairFailure {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub reason: ToolRepairReason,
}

/// Fixed shutdown observations on the existing best-effort diagnostic queue.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShutdownScope {
    Signal,
    Frontend,
    Owner,
}
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShutdownStage {
    SignalTerm,
    SignalHup,
    ConsoleClose,
    ConsoleLogoff,
    ConsoleShutdown,
    TerminalLost,
    SignalForce,
    SignalDeadline,
    Listener,
    DraftEnqueue,
    AcceptedSends,
    DraftSave,
    InterruptCapture,
    ControlTasks,
    ReadWorkers,
    Images,
    Control,
    Workers,
    Owner,
    Database,
    Endpoint,
    Diagnostics,
}
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShutdownBoundary {
    Begin,
    End,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct ShutdownTrace {
    scope: ShutdownScope,
    stage: ShutdownStage,
    boundary: ShutdownBoundary,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<crate::protocol::ErrorCode>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    send: Option<MessageSendTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shutdown: Option<ShutdownTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_repair: Option<ToolRepairFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_slow: Option<UsageSlowTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui_history: Option<UiHistoryTrace>,
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
    send: Option<MessageSendTrace>,
    shutdown: Option<ShutdownTrace>,
    tool_repair: Option<ToolRepairFailure>,
    usage_slow: Option<UsageSlowTrace>,
    ui_history: Option<UiHistoryTrace>,
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
                    send,
                    shutdown,
                    tool_repair,
                    usage_slow,
                    ui_history,
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
                        send,
                        shutdown,
                        tool_repair,
                        usage_slow,
                        ui_history,
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
    /// Nonblocking, fixed-stage diagnostics. Never accepts message or error text.
    pub fn record_message_send(&self, ids: DiagnosticIds, mut send: MessageSendTrace) {
        send.draft_id = sanitize_id(&send.draft_id);
        send.operation_id = sanitize_id(&send.operation_id);
        send.message_id = send.message_id.as_deref().map(sanitize_id);
        self.enqueue_with_details(
            DiagnosticEvent::MessageSend,
            ids,
            None,
            None,
            Some(send),
            None,
        );
    }
    /// Nonblocking observation only: no I/O, DB access or content formatting.
    pub fn record_ui_history(&self, trace: UiHistoryTrace, ids: DiagnosticIds) {
        self.enqueue_record(Record {
            event: DiagnosticEvent::UiHistory,
            ids,
            failure: None,
            stream_difference: None,
            send: None,
            shutdown: None,
            tool_repair: None,
            usage_slow: None,
            ui_history: Some(trace),
        });
    }
    pub(crate) fn record_tool_repair_failure(&self, mut failure: ToolRepairFailure) {
        failure.thread_id = sanitize_id(&failure.thread_id);
        failure.group_id = failure.group_id.as_deref().map(sanitize_id);
        self.enqueue_record(Record {
            event: DiagnosticEvent::DatabaseFailure,
            ids: DiagnosticIds {
                thread_id: Some(failure.thread_id.clone()),
                ..Default::default()
            },
            failure: None,
            stream_difference: None,
            send: None,
            shutdown: None,
            tool_repair: Some(failure),
            usage_slow: None,
            ui_history: None,
        });
    }
    /// No fast-path diagnostic at any level; only fixed non-content fields enter the writer.
    pub(crate) fn record_usage_aggregation(
        &self,
        trigger: UsageTrigger,
        scope: UsageScope,
        elapsed: std::time::Duration,
    ) {
        let Some(usage_slow) = slow_usage_trace(trigger, scope, elapsed) else {
            return;
        };
        self.enqueue_record(Record {
            event: DiagnosticEvent::UsageTotalsSlow,
            ids: DiagnosticIds::default(),
            failure: None,
            stream_difference: None,
            send: None,
            shutdown: None,
            tool_repair: None,
            usage_slow: Some(usage_slow),
            ui_history: None,
        });
    }
    /// Caller supplies monotonic elapsed time; writer latency is not included.
    pub(crate) fn record_shutdown(
        &self,
        scope: ShutdownScope,
        stage: ShutdownStage,
        boundary: ShutdownBoundary,
        elapsed_ms: u64,
    ) {
        self.enqueue_with_details(
            DiagnosticEvent::Shutdown,
            DiagnosticIds::default(),
            None,
            None,
            None,
            Some(ShutdownTrace {
                scope,
                stage,
                boundary,
                elapsed_ms,
                error_code: None,
            }),
        );
    }
    /// Fixed product classification only; never queue the underlying DB/OS
    /// error message, path, or provider text after terminal presentation is lost.
    pub(crate) fn record_shutdown_result(
        &self,
        scope: ShutdownScope,
        stage: ShutdownStage,
        elapsed_ms: u64,
        error_code: Option<crate::protocol::ErrorCode>,
    ) {
        self.enqueue_with_details(
            DiagnosticEvent::Shutdown,
            DiagnosticIds::default(),
            None,
            None,
            None,
            Some(ShutdownTrace {
                scope,
                stage,
                boundary: ShutdownBoundary::End,
                elapsed_ms,
                error_code,
            }),
        );
    }
    fn enqueue(
        &self,
        event: DiagnosticEvent,
        ids: DiagnosticIds,
        failure: Option<ModelFailureDetails>,
        stream_difference: Option<ModelFailureDetails>,
    ) {
        self.enqueue_with_details(event, ids, failure, stream_difference, None, None);
    }
    fn enqueue_with_details(
        &self,
        event: DiagnosticEvent,
        ids: DiagnosticIds,
        failure: Option<ModelFailureDetails>,
        stream_difference: Option<ModelFailureDetails>,
        send: Option<MessageSendTrace>,
        shutdown: Option<ShutdownTrace>,
    ) {
        self.enqueue_record(Record {
            event,
            ids,
            failure,
            stream_difference,
            send,
            shutdown,
            tool_repair: None,
            usage_slow: None,
            ui_history: None,
        });
    }
    fn enqueue_record(&self, record: Record) {
        let Record {
            event,
            mut ids,
            failure,
            stream_difference,
            send,
            shutdown,
            tool_repair,
            usage_slow,
            ui_history,
        } = record;
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
                    send,
                    shutdown,
                    tool_repair,
                    usage_slow,
                    ui_history,
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

#[cfg(test)]
mod ui_history_tests {
    use super::*;

    #[tokio::test]
    async fn history_observations_are_fixed_bounded_and_nonblocking() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(root.path().into(), Logging::default());
        {
            let _busy = diagnostics.shared.sender.lock().unwrap();
            diagnostics.record_ui_history(UiHistoryTrace::default(), DiagnosticIds::default());
            assert_eq!(diagnostics.pending_dropped(), 1);
        }
        for reason in [
            UiHistoryReason::WindowApplied,
            UiHistoryReason::StaleWindow,
            UiHistoryReason::UserFollow,
            UiHistoryReason::BodyEviction,
        ] {
            diagnostics.record_ui_history(
                UiHistoryTrace {
                    reason,
                    generation: 7,
                    view_revision: 8,
                    read_revision: 9,
                    before_nodes: 50,
                    after_nodes: 49,
                    evicted_bodies: 1,
                    follow_before: true,
                    follow_after: false,
                },
                DiagnosticIds {
                    thread_id: Some(format!("thread-{}\n\u{1b}", "x".repeat(1000))),
                    ..Default::default()
                },
            );
        }
        diagnostics.record(DiagnosticEvent::Startup, DiagnosticIds::default());
        diagnostics.shutdown().await;
        assert!(!diagnostics.has_failed());
        let log = std::fs::read_to_string(root.path().join("shellboy.log")).unwrap();
        let entries = log
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 5);
        for (entry, reason) in entries.iter().zip([
            "window_applied",
            "stale_window",
            "user_follow",
            "body_eviction",
        ]) {
            assert_eq!(entry["event"], "ui_history");
            assert_eq!(entry["ui_history"]["reason"], reason);
            assert_eq!(entry["ui_history"]["generation"], 7);
            assert_eq!(entry["ui_history"]["view_revision"], 8);
            assert_eq!(entry["ui_history"]["read_revision"], 9);
            assert_eq!(entry["ui_history"]["before_nodes"], 50);
            assert_eq!(entry["ui_history"]["after_nodes"], 49);
            assert_eq!(entry["ui_history"]["evicted_bodies"], 1);
            assert_eq!(entry["ui_history"]["follow_before"], true);
            assert_eq!(entry["ui_history"]["follow_after"], false);
            assert_eq!(entry["ui_history"].as_object().unwrap().len(), 9);
            let id = entry["ids"]["thread_id"].as_str().unwrap();
            assert_eq!(id.len(), 128);
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_:".contains(c))
            );
            assert_eq!(entry.as_object().unwrap().len(), 5);
        }
        assert_eq!(entries[0]["dropped"], 1);
        assert!(entries[4].get("ui_history").is_none());
    }
}

#[cfg(test)]
mod tool_repair_tests {
    use super::*;
    #[tokio::test]
    async fn skipped_tool_repair_records_only_bounded_ids_and_fixed_reason() {
        let root = tempfile::tempdir().unwrap();
        let logs = root.path().join("logs");
        let diagnostics = Diagnostics::start(
            logs.clone(),
            Logging {
                file_bytes: 8192,
                files: 2,
            },
        );
        diagnostics.record_tool_repair_failure(ToolRepairFailure {
            thread_id: "t".repeat(1000),
            group_id: Some("g".repeat(1000)),
            reason: ToolRepairReason::MissingCallSource,
        });
        diagnostics.shutdown().await;
        assert!(!diagnostics.has_failed());
        let paths = std::fs::read_dir(logs)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 1);
        let body = std::fs::read_to_string(&paths[0]).unwrap();
        let entry: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(entry["tool_repair"]["reason"], "missing_call_source");
        assert!(entry["tool_repair"]["thread_id"].as_str().unwrap().len() <= 128);
        assert!(entry["tool_repair"]["group_id"].as_str().unwrap().len() <= 128);
        assert!(entry.get("body").is_none());
        assert!(entry.get("message").is_none());
    }
}

#[cfg(test)]
mod shutdown_result_tests {
    use super::*;
    #[tokio::test]
    async fn failed_control_drain_keeps_fixed_error_code_after_writer_close() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(
            root.path().to_owned(),
            Logging {
                file_bytes: 8192,
                files: 2,
            },
        );
        diagnostics.record_shutdown_result(
            ShutdownScope::Frontend,
            ShutdownStage::Control,
            42,
            Some(crate::protocol::ErrorCode::SaveFailed),
        );
        diagnostics.shutdown().await;
        let body = std::fs::read_to_string(root.path().join("shellboy.log")).unwrap();
        let record: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(record["shutdown"]["error_code"], "save_failed");
        assert_eq!(record["shutdown"]["boundary"], "end");
        assert_eq!(record["shutdown"]["elapsed_ms"], 42);
        assert!(record.get("message").is_none());
        assert!(record.get("path").is_none());
    }
}

#[cfg(test)]
mod usage_slow_tests {
    use super::*;
    #[tokio::test]
    async fn only_at_least_100ms_aggregations_reach_the_existing_log() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(root.path().into(), Logging::default());
        for duration in [
            std::time::Duration::ZERO,
            std::time::Duration::from_micros(99_999),
        ] {
            assert!(
                slow_usage_trace(UsageTrigger::Changed, UsageScope::Thread, duration).is_none()
            );
            diagnostics.record_usage_aggregation(
                UsageTrigger::Changed,
                UsageScope::Thread,
                duration,
            );
        }
        diagnostics.record_usage_aggregation(
            UsageTrigger::Open,
            UsageScope::Thread,
            std::time::Duration::from_millis(100),
        );
        diagnostics.record_usage_aggregation(
            UsageTrigger::Cli,
            UsageScope::Turn,
            std::time::Duration::from_millis(101),
        );
        diagnostics.shutdown().await;
        let text = std::fs::read_to_string(root.path().join("shellboy.log")).unwrap();
        let rows = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["event"], "usage_totals_slow");
        assert_eq!(rows[0]["usage_slow"]["elapsed_ms"], 100);
        assert_eq!(rows[1]["usage_slow"]["elapsed_ms"], 101);
        assert_eq!(rows[0]["usage_slow"].as_object().unwrap().len(), 3);
        assert!(
            rows.iter()
                .all(|row| row["ids"].as_object().unwrap().is_empty())
        );
    }
}
