//! Versioned local-operation contracts shared by CLI, Control, and TUI.
mod notification;
pub use notification::{
    InstructionObservation, NotificationDescriptor, NotificationSetting, NotificationTerminal,
};
mod sender;
pub use sender::{MessageSender, SenderKind, validate_sender_label};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u32 = 42;
pub const PROTOCOL_CAPABILITIES: &[&str] = &["events.reasoning_activity", "id.resolve"];
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PENDING_REPLY_DUTIES: u64 = 16;
pub const MAX_SYNC_DELETION_REPLY_DUTIES: u64 = 16;
pub const MAX_HUMAN_REPLY_SUMMARY_DUTIES: u64 = 16;
pub const DEFAULT_LIST_LIMIT: u32 = 50;
pub const MAX_LIST_LIMIT: u32 = 200;
pub const DEFAULT_HISTORY_EXCERPT_CHARS: u32 = 512;
pub const MAX_HISTORY_EXCERPT_CHARS: u32 = 4096;
pub const MAX_BODY_BYTES: u32 = 64 * 1024;
pub const DEFAULT_SEARCH_LIMIT: u32 = 10;
pub const MAX_SEARCH_LIMIT: u32 = 50;
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_ENCODING: &str = "UTF-8";

/// JSON Schema document for the stable RPC envelope and its generated wire types.
fn operation_result_schemas(
    generator: &mut schemars::generate::SchemaGenerator,
) -> serde_json::Value {
    let mut results = serde_json::Map::new();
    macro_rules! opaque {
        ($name:literal) => {
            results.insert($name.to_owned(), serde_json::Value::Bool(true));
        };
    }
    macro_rules! result {
        ($name:literal, $ty:ty) => {
            results.insert(
                $name.to_owned(),
                serde_json::to_value(generator.root_schema_for::<$ty>())
                    .expect("schema serializes"),
            );
        };
    }
    result!("draft.image", DraftImageResult);
    result!("owner.shutdown", Accepted);
    result!("draft.save", Accepted);
    result!("draft.get", DraftState);
    result!("draft.list", DraftListPage);
    result!("draft.restore", crate::draft::DraftSnapshot);
    result!("draft.submit", MessageAccepted);
    result!("status", StatusResult);
    result!("model.list", ModelListResult);
    result!("thread.list", ThreadListPage);
    result!("history.export", HistoryExportPage);
    result!("thread.get", ThreadDetails);
    result!(
        "thread.branches",
        crate::history::branches::BranchListResult
    );
    result!("usage.get", UsageGetResult);
    result!("compact.list", CompactListResult);
    result!("shell.list", ShellListResult);
    opaque!("thread.create");
    result!("thread.import", ThreadImportResult);
    opaque!("thread.fork");
    result!("thread.delete", ThreadDeleteResult);
    opaque!("thread.delete_preview");
    opaque!("thread.delete_set_preview");
    result!("thread.delete_set", Accepted);
    opaque!("thread.prune");
    opaque!("thread.rewind");
    result!("thread.archive", ThreadArchiveResult);
    result!("thread.unarchive", ThreadUnarchiveResult);
    opaque!("thread.tag");
    result!("thread.tags", RecordPage<String, NoListFilters>);
    result!("artifact.add", ArtifactSelected);
    result!("artifact.update", ArtifactSelected);
    result!("artifact.list", RecordPage<ArtifactSummary, ThreadScopeFilter>);
    result!("artifact.get", ArtifactGetResult);
    result!("artifact.remove", Accepted);
    opaque!("schedule.add");
    opaque!("schedule.list");
    opaque!("schedule.set");
    opaque!("schedule.enable");
    opaque!("schedule.disable");
    opaque!("schedule.remove");

    result!("thread.cwds", RecordPage<String, NoListFilters>);
    result!("thread.set", ThreadDetails);
    result!("id.resolve", IdResolveResult);
    result!("message.ask", MessageAccepted);
    result!("message.reply", MessageReplied);
    result!("message.send", MessageAccepted);
    result!("message.get", MessageGetResult);
    result!("message.list", MessageListPage);
    result!("message.cancel", MessageCancelResult);
    result!("operation.get", OperationReceiptDetails);
    result!("run.get", RunDetails);
    opaque!("run.retry");
    opaque!("run.stop");
    result!("turn.get", TurnDetails);
    result!("turn.interrupt", TurnInterruptResult);
    result!("wait", WaitResult);
    result!("events.wait", EventsWaitResult);
    result!("history.list", HistoryListPage);
    result!("history.window", HistoryWindow);
    result!("history.recall", HistoryRecallResult);
    result!("history.received", HistoryReceivedPage);
    result!("history.read", HistoryBody);
    result!("history.search", Page<SearchMatch>);
    result!("history.relations", RelationListPage);
    result!("shell.get", ShellDetails);
    result!("shell.read", ShellOutput);
    result!("shell.write", ShellWriteResult);
    result!("shell.eof", Accepted);
    result!("shell.background", ShellBackgroundResult);
    result!("shell.stop", ShellStopResult);
    result!("attachment.add", AttachmentDetails);
    result!("attachment.list", RecordPage<AttachmentDetails, ThreadScopeFilter>);
    result!("attachment.reload", AttachmentReloadResult);
    result!("attachment.remove", AttachmentRemoveResult);
    opaque!("compact.preview");
    opaque!("compact");
    opaque!("context.edit");
    result!("index.status", IndexStatus);
    opaque!("index.preview");
    opaque!("index.backfill");
    opaque!("index.retry");
    opaque!("index.rebuild");
    result!("index.probe", IndexProbeObservation);
    result!("index.shrink", IndexShrinkStatus);
    opaque!("index.remove");
    result!("storage.status", StorageStatus);
    opaque!("storage.collect");
    result!("config.get", ConfigGetResult);
    result!("config.set", ConfigSetResult);
    result!("credentials.set", CredentialsSetResult);
    result!("credentials.clear", CredentialsClearResult);
    opaque!("connection.test");
    serde_json::Value::Object(results)
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShellBackgroundResult {
    pub execution_id: String,
    pub requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TurnInterruptResult {
    pub thread_id: String,
    pub run_id: String,
    pub turn_id: String,
    pub requested: bool,
    pub already_requested: bool,
    pub state: TurnInterruptState,
    pub interrupt_requested_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnInterruptState {
    InterruptRequested,
}

/// Existing `operation.get` projection; saved outcomes are operation-specific JSON.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct OperationReceiptDetails {
    pub operation_id: String,
    pub instance_id: String,
    pub created_at: String,
    pub current_instance: bool,
    pub state: String,
    pub outcome: Value,
    pub result_uncertain: bool,
    pub result_expired: bool,
    pub replay_policy: OperationReplayPolicy,
    pub targets: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downstream: Option<OperationReceiptDownstream>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationReplayPolicy {
    DoNotReexecute,
    OperationExpired,
    ReturnSavedOutcome,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationReceiptDownstream {
    Message {
        state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hold_reason: Option<String>,
    },
    Compact {
        state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ended_at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result_uncertain: Option<bool>,
    },
}

pub fn schema_json() -> String {
    let mut settings = schemars::generate::SchemaSettings::draft2020_12();
    settings.definitions_path = "/definitions".into();
    let mut generator = settings.into_generator();
    let mut definitions = serde_json::Map::new();
    macro_rules! add_root {
        ($name:literal, $ty:ty) => {
            definitions.insert(
                $name.to_owned(),
                serde_json::to_value(generator.root_schema_for::<$ty>())
                    .expect("schema serializes"),
            );
        };
    }
    let command_schema = generator.root_schema_for::<Command>();
    let command_schema_value = serde_json::to_value(&command_schema).expect("schema serializes");
    definitions.insert("Command".into(), command_schema_value.clone());
    let mut request_options = command_schema_value
        .as_object()
        .and_then(|object| object.get("oneOf"))
        .and_then(serde_json::Value::as_array)
        .expect("command is an adjacently tagged enum")
        .clone();
    for option in &mut request_options {
        let variant = option
            .as_object_mut()
            .expect("command variant is an object");
        let properties = variant
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
            .expect("command variant has properties");
        properties.insert(
            "protocol_version".into(),
            serde_json::json!({"type":"integer","format":"uint32","minimum":0}),
        );
        properties.insert(
            "operation_id".into(),
            serde_json::json!({"type":"string","minLength":1}),
        );
        properties.insert("origin".into(), serde_json::Value::Bool(true));
        variant.insert(
            "additionalProperties".into(),
            serde_json::Value::Bool(false),
        );
        let required = variant
            .get_mut("required")
            .and_then(serde_json::Value::as_array_mut)
            .expect("command variant has required fields");
        required.extend(
            ["protocol_version", "operation_id"]
                .into_iter()
                .map(serde_json::Value::from),
        );
        required.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        required.dedup();
    }
    let request = serde_json::json!({"oneOf": request_options});
    definitions.insert("Request".into(), request);
    add_root!("Response", Response<Value>);
    definitions.insert("Command".into(), command_schema_value);
    add_root!("Error", ApiError);
    add_root!("Origin", Origin);
    add_root!("PageCursor", PageCursor);
    add_root!("EventCursor", EventCursor);
    add_root!("EventsWaitResult", EventsWaitResult);
    add_root!("Hello", crate::ipc::Hello);
    add_root!("Welcome", crate::ipc::Welcome);
    add_root!("Endpoint", crate::app::Endpoint);
    add_root!("Accepted", Accepted);
    add_root!("ModelListResult", ModelListResult);
    add_root!("MessageAccepted", MessageAccepted);
    add_root!("MessageCancelResult", MessageCancelResult);
    add_root!("Page", ThreadListPage);
    add_root!("ThreadSummary", ThreadSummary);
    add_root!("ThreadDetails", ThreadDetails);
    add_root!("ThreadActivity", ThreadActivity);
    add_root!("MessageDetails", MessageDetails);
    add_root!("RunDetails", RunDetails);
    add_root!("TurnDetails", TurnDetails);
    add_root!("WaitResult", WaitResult);
    add_root!("HistorySnippet", HistorySnippet);
    add_root!("HistoryWindow", HistoryWindow);
    add_root!("HistoryRecallResult", HistoryRecallResult);
    add_root!("HistoryBody", HistoryBody);
    add_root!("SearchMatch", SearchMatch);
    add_root!("RelationItem", RelationItem);
    add_root!("ShellDetails", ShellDetails);
    add_root!("ShellOutput", ShellOutput);
    add_root!("ShellWriteStatus", ShellWriteStatus);
    add_root!("ShellWriteResult", ShellWriteResult);
    add_root!("AttachmentDetails", AttachmentDetails);
    add_root!("IndexStatus", IndexStatus);
    add_root!("IndexProbeObservation", IndexProbeObservation);
    add_root!("IndexShrinkStatus", IndexShrinkStatus);
    add_root!("StorageStatus", StorageStatus);
    add_root!("StatusSnapshot", crate::status::StatusSnapshot);
    add_root!("WorkItem", crate::status::WorkItem);
    add_root!("ResponseNodeRef", crate::status::ResponseNodeRef);
    add_root!("StatusFilters", crate::status::StatusFilters);
    add_root!("StatusTarget", StatusTarget);
    add_root!("StatusReply", StatusReply);
    add_root!("ReceiptBudget", ReceiptBudget);
    add_root!("StatusResult", StatusResult);
    add_root!("StatusMemoryResult", StatusMemoryResult);
    add_root!("StatusMemoryProcess", StatusMemoryProcess);
    add_root!("StatusMemoryCache", StatusMemoryCache);
    add_root!("StatusMemoryMetric", StatusMemoryMetric);
    add_root!("ShellListItem", ShellListItem);
    add_root!("ShellWait", ShellWait);
    add_root!("ShellListResult", ShellListResult);
    add_root!("TimeWindowResult", TimeWindowResult);
    add_root!("UsageNumber", UsageNumber);
    add_root!("UsageAttemptTotals", UsageAttemptTotals);
    add_root!("UsageAttemptPeriod", UsageAttemptPeriod);
    add_root!("UsageCoverage", UsageCoverage);
    add_root!("UsageTiming", UsageTiming);
    add_root!("UsageActiveAttempt", UsageActiveAttempt);
    add_root!("UsageActive", UsageActive);
    add_root!("CompactionUsageResult", CompactionUsageResult);
    add_root!("UsageAggregateResult", UsageAggregateResult);
    add_root!("PromptCacheDiagnostics", PromptCacheDiagnostics);
    add_root!("UsageAttemptPayload", UsageAttemptPayload);
    add_root!("UsageInputTokenDetails", UsageInputTokenDetails);
    add_root!("UsageOutputTokenDetails", UsageOutputTokenDetails);
    add_root!("UsageInputBytes", UsageInputBytes);
    add_root!("UsageAttemptResult", UsageAttemptResult);
    add_root!("UsageBreakdownCoverage", UsageBreakdownCoverage);
    add_root!("UsageBreakdownResult", UsageBreakdownResult);
    add_root!("UsageGetResult", UsageGetResult);
    add_root!("ConfigLoadedView", ConfigLoadedView);
    add_root!("ConfigFileView", ConfigFileView);
    add_root!("ConfigCredentialsFileView", ConfigCredentialsFileView);
    add_root!("ConfigRestartView", ConfigRestartView);
    add_root!("ConfigGetResult", ConfigGetResult);
    let operation_results = operation_result_schemas(&mut generator);
    for (name, schema) in generator.take_definitions(false) {
        definitions.entry(name).or_insert(schema);
    }
    // Every fragment shares the document root, so keep referenced definitions there.
    fn remove_nested_definitions(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                object.remove("definitions");
                for child in object.values_mut() {
                    remove_nested_definitions(child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    remove_nested_definitions(child);
                }
            }
            _ => {}
        }
    }
    for schema in definitions.values_mut() {
        remove_nested_definitions(schema);
    }
    let mut operation_results = operation_results;
    remove_nested_definitions(&mut operation_results);
    let document = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://shellboy.dev/schema/protocol-v{PROTOCOL_VERSION}.json"),
        "title": format!("Shellboy local RPC protocol v{PROTOCOL_VERSION}"),
        "protocol_version": PROTOCOL_VERSION,
        "capabilities": PROTOCOL_CAPABILITIES,
        "operation_results": operation_results,
        "opaque_result_note": "A true operation schema means the result is opaque JSON, not yet represented by a stable Rust result record.",
        "definitions": definitions
    });
    serde_json::to_string_pretty(&document).expect("schema document serializes")
}

macro_rules! states {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub enum $name { $(#[serde(rename = $wire)] $variant),+ }
        impl $name {
            pub const VALUES: &'static [Self] = &[$(Self::$variant),+];
            pub const NAMES: &'static [&'static str] = &[$($wire),+];
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
            pub fn parse(value: &str) -> Option<Self> {
                match value { $($wire => Some(Self::$variant)),+, _ => None }
            }
        }
    };
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}
states!(ErrorCode {
    InvalidArgument,
    SelfWait,
    ConfigInvalid,
    AppNotRunning,
    ProtocolMismatch,
    ThreadNotFound,
    TargetUnavailable,
    NotFound,
    OperationConflict,
    OperationExpired,
    OperationNotFound,
    InvalidState,
    PreviewStale,
    CursorExpired,
    CursorEncodingMismatch,
    OutputExpired,
    InputLimit,
    ReplyLimit,
    StorageLimit,
    SaveFailed,
    IoFailed,
    ModelAuth,
    ModelRequestRejected,
    ModelRequestFailed
});
states!(RunState {
    Live,
    Stopping,
    Stopped,
    Completed,
    Failed
});
states!(TurnState {
    Preparing,
    Running,
    Waiting,
    RecoveryWait,
    Completed,
    Stopped,
    Failed
});
states!(AttemptState {
    Preparing,
    Inflight,
    Completed,
    Failed,
    Incomplete,
    Cancelled
});
states!(ShellState {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
    Stopped
});
states!(DeliveryStatus {
    Accepted,
    Held,
    Applied,
    Cancelled
});
states!(Assignment {
    NewRun,
    NewTurn,
    ExistingTurn,
    Pending
});
states!(WaitStatus {
    Satisfied,
    Timeout,
    Interrupted,
    RecoveryWait,
    Unavailable
});
states!(EventsWaitStatus {
    Baseline => "baseline", Changed => "changed", Text => "text", Timeout => "timeout", ResyncRequired => "resync_required"
});
states!(AttachmentKind { Image => "image", File => "file" });
states!(StorageCategory {
    Raw => "raw", ManagedImages => "managed_images", History => "history", Index => "index", All => "all"
});
states!(IndexScope { Threads => "threads", All => "all" });

#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, thiserror::Error,
)]
#[error("{message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub details: Option<Value>,
    // In-memory capability only: provider JSON and persisted errors cannot mint it.
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) compaction_cause: Option<Box<crate::compaction::discard::Outcome>>,
}
impl ApiError {
    /// Only source-store failure closes admission and stops owned resources.
    pub fn is_storage_failure(&self) -> bool {
        self.code == ErrorCode::SaveFailed
    }

    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            details: None,
            compaction_cause: None,
        }
    }
    pub(crate) fn precondition_changed(
        fact: &'static str,
        current: Option<Value>,
        message: &'static str,
    ) -> Self {
        let mut details = serde_json::json!({
            "reason": format!("expected_{fact}_changed"),
            "request_may_have_been_applied": false,
        });
        if let Some(current) = current {
            details["current"] = current;
        }
        Self::new(ErrorCode::OperationConflict, message)
            .hint("Read the current value, review the change, and use a new operation ID if you still want to act")
            .details(details)
    }

    /// Conservative recovery guidance. Specific, state-aware hints take precedence.
    pub fn with_recovery_hint(mut self) -> Self {
        if self.hint.is_some() {
            return self;
        }
        self.hint = Some(match self.code {
            ErrorCode::InvalidArgument => "Read shellboy <command> --help; find English guide examples with shellboy docs search --query 'background'",
            ErrorCode::ConfigInvalid => "Inspect the selected configuration and shellboy docs get --id shellboy/configuration; do not repeat an unconfirmed mutation",
            ErrorCode::AppNotRunning => "Check --data-dir and start its owner if needed; query any known operation receipt before repeating a mutation",
            ErrorCode::ProtocolMismatch => "Check the selected owner and client version; inspect any known operation receipt before repeating a mutation",
            ErrorCode::ThreadNotFound | ErrorCode::TargetUnavailable | ErrorCode::NotFound => "Check the returned target ID and --data-dir with the corresponding get command; do not infer a replacement ID",
            ErrorCode::OperationConflict => "Query the existing receipt with shellboy operation get --operation OPERATION_ID; do not change the ID to repeat the same side effect",
            ErrorCode::OperationExpired | ErrorCode::OperationNotFound => "Inspect saved target state and history; an unavailable receipt does not prove the mutation was never applied",
            ErrorCode::InvalidState => "Inspect the target with its get command; read shellboy docs get --id shellboy/recovery-and-storage before choosing retry or stop",
            ErrorCode::PreviewStale => "Request the corresponding deletion preview again and explicitly confirm its new target marker; never retry deletion with the stale preview",

            ErrorCode::CursorExpired => "Restart the same list/read with its original target and filters, without the old cursor; previously returned items or bytes may repeat",
            ErrorCode::CursorEncodingMismatch => "Restart shell read without the old cursor and specify the desired encoding; previously read bytes may be returned again",
            ErrorCode::OutputExpired => "For rotated output, try shell read without a cursor; unavailable raw bytes cannot be reconstructed",
            ErrorCode::SelfWait => "Observe another Run, Turn or shell; do not wait for the invoking resource itself",
            ErrorCode::InputLimit => "Inspect the input bound in command help; do not treat truncated input as accepted",
            ErrorCode::ReplyLimit => "Withdraw an outstanding request you sent, or wait for a reply before asking again. This request was not accepted",
            ErrorCode::StorageLimit => "Inspect shellboy storage status and storage capacity; resolve the cause before explicitly choosing a recovery action",
            ErrorCode::SaveFailed => "Preserve unsaved input and inspect capacity/permissions; do not keep submitting mutations while saving is unavailable",
            ErrorCode::ModelAuth => "Check the selected connection with shellboy connection test --connection NAME",
            ErrorCode::ModelRequestRejected => "Inspect the selected connection and model with shellboy config get",
            ErrorCode::ModelRequestFailed => "Inspect the saved Turn with shellboy turn get --turn ID before retrying",
            ErrorCode::IoFailed => "Inspect any known operation receipt and saved target state; read shellboy docs get --id shellboy/recovery-and-storage before repeating a side effect",
        }.into());
        self
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}
fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}
fn nonempty(value: &str, field: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        Err(invalid(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}
fn optional_nonempty(value: Option<&str>, field: &str) -> Result<(), ApiError> {
    value.map_or(Ok(()), |v| nonempty(v, field))
}

/// Token is deliberately excluded from Debug to avoid logging shell credentials.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Origin {
    pub instance_id: String,
    pub thread_id: String,
    pub run_id: String,
    pub turn_id: String,
    pub execution_id: String,
    pub token: String,
}
/// Credential-free provenance for persistence and human/model projections.
/// This is not an authentication credential: only `Origin` can cross the IPC
/// authentication boundary, where its token is checked against issued origins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OriginIdentity {
    pub instance_id: String,
    pub thread_id: String,
    pub run_id: String,
    pub turn_id: String,
    pub execution_id: String,
}
impl Origin {
    pub fn identity(&self) -> OriginIdentity {
        OriginIdentity {
            instance_id: self.instance_id.clone(),
            thread_id: self.thread_id.clone(),
            run_id: self.run_id.clone(),
            turn_id: self.turn_id.clone(),
            execution_id: self.execution_id.clone(),
        }
    }
}
impl std::fmt::Debug for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Origin")
            .field("instance_id", &self.instance_id)
            .field("execution_id", &self.execution_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Request {
    pub protocol_version: u32,
    pub operation_id: String,
    #[serde(flatten)]
    pub command: Command,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}
// Both entry points accept exactly the same top-level envelope fields. Keep
// static diagnostics here: arbitrary keys/values can contain credentials.
const REQUEST_FIELDS: &[&str] = &[
    "protocol_version",
    "operation_id",
    "command",
    "args",
    "origin",
];
fn request_fields(value: &Value) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("request must be an object")?;
    if object
        .keys()
        .any(|key| !REQUEST_FIELDS.contains(&key.as_str()))
    {
        return Err("unknown request field");
    }
    Ok(())
}
/// Bounded, printable form of a caller-supplied name for diagnostics.
fn diagnostic_name(name: &str) -> String {
    name.chars()
        .take(64)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}
impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = Value::deserialize(deserializer)?;
        request_fields(&value).map_err(D::Error::custom)?;
        #[derive(Deserialize)]
        struct Wire {
            protocol_version: u32,
            operation_id: String,
            #[serde(flatten)]
            command: Command,
            #[serde(default)]
            origin: Option<Origin>,
        }
        let wire: Wire = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self {
            protocol_version: wire.protocol_version,
            operation_id: wire.operation_id,
            command: wire.command,
            origin: wire.origin,
        })
    }
}
impl Request {
    pub fn new(operation_id: impl Into<String>, command: Command) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            operation_id: operation_id.into(),
            command,
            origin: None,
        }
    }
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ApiError::new(
                ErrorCode::ProtocolMismatch,
                "unsupported local protocol version",
            ));
        }
        nonempty(&self.operation_id, "operation_id")?;
        if let Some(origin) = &self.origin {
            for (name, value) in [
                ("instance_id", &origin.instance_id),
                ("thread_id", &origin.thread_id),
                ("run_id", &origin.run_id),
                ("turn_id", &origin.turn_id),
                ("execution_id", &origin.execution_id),
                ("token", &origin.token),
            ] {
                nonempty(value, name)?;
            }
        }
        self.command.validate()
    }
    /// Decode a request frame that was already parsed as JSON. On failure the
    /// caller gets the request's own non-empty `operation_id` (when it has one)
    /// and a typed `invalid_argument` naming the unknown command or field, so
    /// the owner can reply instead of closing. Nothing is executed here.
    /// Diagnostics never echo argument values, only bounded command/field names.
    pub fn decode_frame(value: Value) -> Result<Self, (Option<String>, ApiError)> {
        let operation_id = value
            .get("operation_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned);
        let command = value
            .get("command")
            .and_then(Value::as_str)
            .map(diagnostic_name);
        let fail = |message: String, kind: &str, name: Option<String>| {
            let mut details = serde_json::json!({
                "reason": kind,
                "request_may_have_been_applied": false
            });
            if let Some(name) = name {
                details["name"] = Value::String(name);
            }
            if let Some(command) = &command {
                details["command"] = Value::String(command.clone());
            }
            invalid(message)
                .hint("The owner rejected the request before running anything. The client and owner may be from different releases; run them from the same release, then retry")
                .details(details)
        };
        if request_fields(&value).is_err() {
            let name = value
                .as_object()
                .into_iter()
                .flat_map(|object| object.keys())
                .find(|key| !REQUEST_FIELDS.contains(&key.as_str()))
                .map(|key| diagnostic_name(key));
            let message = match &name {
                Some(name) => format!("unknown request field `{name}`"),
                None => "request must be an object".into(),
            };
            let reason = if name.is_some() {
                "unknown_request_field"
            } else {
                "invalid_shape"
            };
            return Err((operation_id, fail(message, reason, name)));
        }
        match serde_json::from_value::<Self>(value) {
            Ok(request) => Ok(request),
            Err(error) => {
                let text = error.to_string();
                let quoted = |prefix: &str| {
                    text.split_once(prefix)
                        .and_then(|(_, rest)| rest.split_once('`'))
                        .map(|(name, _)| diagnostic_name(name))
                };
                let error = if let Some(name) = quoted("unknown variant `") {
                    fail(
                        format!("unknown command `{name}`"),
                        "unknown_command",
                        Some(name),
                    )
                } else if let Some(name) = quoted("unknown field `") {
                    let message = match &command {
                        Some(command) => {
                            format!("unknown field `{name}` in the arguments of `{command}`")
                        }
                        None => format!("unknown field `{name}`"),
                    };
                    fail(message, "unknown_field", Some(name))
                } else {
                    let message = match &command {
                        Some(command) => format!(
                            "invalid arguments for `{command}`: wrong type or missing required field"
                        ),
                        None => "invalid request shape".into(),
                    };
                    fail(message, "invalid_shape", None)
                };
                Err((operation_id, error))
            }
        }
    }
    /// Strict top-level parsing is explicit because serde flatten cannot deny unknown fields.
    pub fn from_value(value: Value) -> Result<Self, ApiError> {
        request_fields(&value).map_err(invalid)?;
        let request: Self =
            serde_json::from_value(value).map_err(|_| invalid("invalid request shape"))?;
        request.validate()?;
        Ok(request)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Response<T> {
    pub protocol_version: u32,
    pub operation_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}
impl<T> Response<T> {
    pub fn success(operation_id: impl Into<String>, result: T) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            operation_id: operation_id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    pub fn failure(operation_id: impl Into<String>, error: ApiError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            operation_id: operation_id.into(),
            ok: false,
            result: None,
            error: Some(error.with_recovery_hint()),
        }
    }
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ApiError::new(
                ErrorCode::ProtocolMismatch,
                "unsupported response protocol version",
            ));
        }
        nonempty(&self.operation_id, "operation_id")?;
        if (self.ok && self.result.is_some() && self.error.is_none())
            || (!self.ok && self.error.is_some() && self.result.is_none())
        {
            Ok(())
        } else {
            Err(invalid(
                "response must contain exactly the matching result or error",
            ))
        }
    }
}

macro_rules! args {
    ($(#[$struct_meta:meta])* $name:ident { $($(#[$meta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$struct_meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($(#[$meta])* pub $field: $ty),* }
    };
}
macro_rules! record {
    ($(#[$struct_meta:meta])* $name:ident { $($(#[$meta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$struct_meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        pub struct $name { $($(#[$meta])* pub $field: $ty),* }
    };
}
pub const ARTIFACT_PAGE_LIMIT: u32 = 32;
fn artifact_page_limit() -> u32 {
    ARTIFACT_PAGE_LIMIT
}
args!(ArtifactAddArgs { thread_id: String, uri: String, title: String, #[serde(default)] mime: Option<String> });
args!(ArtifactUpdateArgs { artifact_id: String, uri: String, #[serde(default)] title: Option<String>, #[serde(default)] mime: Option<String> });
args!(ArtifactRemoveArgs {
    artifact_id: String
});
args!(ArtifactListArgs { thread_id: String, #[serde(default)] cursor: Option<String>, #[serde(default="artifact_page_limit")] limit: u32 });
args!(ArtifactGetArgs { artifact_id: String, #[serde(default)] version: Option<i64>, #[serde(default)] cursor: Option<String>, #[serde(default)] limit: Option<u32> });
record!(ArtifactVersion { version: i64, uri: String, title: String, mime: Option<String>, created_at: String });
record!(ArtifactVersionStamp {
    version: i64,
    created_at: String
});
record!(ArtifactSummary {
    artifact_id: String,
    thread_id: String,
    created_at: String,
    updated_at: String,
    latest: ArtifactVersion
});
record!(ArtifactSelected {
    artifact_id: String,
    thread_id: String,
    created_at: String,
    updated_at: String,
    version: ArtifactVersion
});
record!(ArtifactHistory { #[serde(flatten)] artifact: ArtifactSummary, versions: RecordPage<ArtifactVersionStamp, ArtifactScopeFilter> });
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ArtifactGetResult {
    Selected(ArtifactSelected),
    History(ArtifactHistory),
}

args!(ThreadGetArgs {
    #[serde(default)] thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")] display: bool
});
record!(ThreadDisplayCoverage {
    #[serde(default, skip_serializing_if = "Option::is_none")] title: Option<ThreadTitleFallback>,
    #[serde(default, skip_serializing_if = "is_false")] recovery_limited: bool,
});
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadTitleFallback {
    Limited,
    Untitled,
    NotSent,
}
args!(AttachmentReloadArgs { thread_id: String });

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentReloadResult {
    pub thread_id: String,
    pub selection_revision: i64,
    pub attachments: Vec<crate::attachments::AttachmentManifest>,
    pub notices: Vec<String>,
    pub would_exclude: Vec<String>,
    pub sent: bool,
}

args!(ThreadTarget { #[serde(default)] thread_id: Option<String> });
args!(AttachmentListArgs { #[serde(default)] thread_id: Option<String>, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String> });
args!(#[derive(Default)] UsageGetArgs { #[serde(default)] attempt_id: Option<Box<str>>, #[serde(default)] since: Option<Box<str>>, #[serde(default)] since_node_id: Option<Box<str>>, #[serde(default)] until: Option<String>, #[serde(default)] thread_id: Option<Box<str>>, #[serde(default)] run_id: Option<String>, #[serde(default)] turn_id: Option<String>, #[serde(default)] include_children: bool, #[serde(default)] group_by: Option<Box<str>>, #[serde(default)] timezone: Option<Box<str>>, #[serde(default)] limit: Option<u32>, #[serde(default)] cursor: Option<Box<str>> });
args!(CompactListArgs { #[serde(default)] thread_id:Option<String>, #[serde(default)] since:Option<String>, #[serde(default)] until:Option<String>, #[serde(default="list_limit")] limit:u32 });
args!(ShellListArgs { #[serde(default)] thread_id:Option<String>, #[serde(default)] since:Option<String>, #[serde(default)] until:Option<String>, #[serde(default)] command_contains:Option<String>, #[serde(default)] state:Option<String>, #[serde(default)] exit_code:Option<i32>, #[serde(default="list_limit")] limit:u32, #[serde(default)] cursor:Option<String> });
states!(ContextEditAction {
    Spill,
    Summary,
    Trim,
    Omit,
    Restore,
    Maximal
});
args!(ContextEditArgs {
    #[serde(default)] thread_id: Option<String>,
    action: ContextEditAction,
    #[serde(default)] from_node_id: Option<String>,
    #[serde(default)] to_node_id: Option<String>,
    #[serde(default)] max_chars: Option<u64>
});
impl ContextEditArgs {
    /// The single construction point for optional request controls. Builders
    /// and fixtures set only their explicit overrides after constructing it.
    pub fn for_range(
        action: ContextEditAction,
        thread_id: Option<String>,
        from_node_id: Option<String>,
        to_node_id: Option<String>,
    ) -> Self {
        Self {
            thread_id,
            action,
            from_node_id,
            to_node_id,
            max_chars: None,
        }
    }
}

args!(#[derive(Default)] CompactPreviewArgs {
    #[serde(default)] thread_id: Option<String>,
    #[serde(default)] action: Option<ContextEditAction>,
    #[serde(default)] from_node_id: Option<String>,
    #[serde(default)] to_node_id: Option<String>,
    #[serde(default)] max_chars: Option<u64>
});
args!(ModelListArgs { #[serde(default)] connection: Option<String>, #[serde(default)] thread_id: Option<String> });
record!(ModelListResult { connection: String, models: Vec<String>, api: String, unavailable_features: Vec<String> });
args!(ThreadImportArgs { document: String });
record!(ThreadImportResult { thread_id: String });
args!(ThreadCreateArgs { #[serde(default, skip_serializing_if = "Option::is_none")] agents: Option<String>, #[serde(default)] connection: Option<String>, #[serde(default)] cwd: Option<String>, #[serde(default)] model: Option<String>, #[serde(default)] effort: Option<String>, #[serde(default)] parent_thread_id: Option<String> });
args!(ThreadForkArgs { #[serde(default, skip_serializing_if = "Option::is_none")] agents: Option<String>, thread_id: String, node_id: String, #[serde(default)] cwd: Option<String> });
args!(DeletionPreviewMarker {
    thread_count: u32,
    ids_hash: String
});
args!(ThreadDeleteArgs {
    thread_id: String,
    #[serde(default)]
    descendants: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview_marker: Option<DeletionPreviewMarker>
});
args!(ThreadDeleteSetCommitArgs { thread_ids: Vec<String>, preview_marker: DeletionPreviewMarker });
args!(ThreadDeleteSetArgs { thread_ids: Vec<String> });
args!(ThreadPruneArgs {
    #[serde(default)]
    thread_id: Option<String>,
    before_node_id: String,
    #[serde(default, skip_serializing_if = "is_false")]
    dry_run: bool
});
args!(ThreadBranchesArgs {
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default = "list_limit")]
    limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>
});
// Explicit Thread IDs to archive or unarchive; empty means the calling Thread (#222).
args!(ThreadArchiveArgs {
    #[serde(default)]
    thread_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    descendants: bool
});
// Tags to add to and remove from explicit Threads; empty IDs mean the calling Thread (#222 stage 3).
args!(ThreadTagArgs {
    #[serde(default)]
    thread_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remove: Vec<String>
});
// Distinct tag names in byte order, paged; no member counts.
args!(ServiceTarget { service_id: String });
args!(ServiceAddArgs { thread_id: Option<String>, cwd: Option<String>, description: Option<String>, command: String });
args!(ScheduleTarget {
    schedule_id: String
});
args!(ScheduleSetArgs {
    schedule_id: String,
    description: String
});
args!(ScheduleAddArgs { thread_id: Option<String>, cron: Option<String>, at: Option<String>, in_seconds: Option<u64>, cwd: Option<String>, description: Option<String>, command: String });
args!(ThreadTagsArgs {
    #[serde(default = "list_limit")]
    limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>
});
// Distinct saved cwds of non-deleted Threads (archived included) in byte order, paged; no counts.
args!(ThreadCwdsArgs {
    #[serde(default = "list_limit")]
    limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>
});
args!(ThreadRewindArgs {
    #[serde(default)]
    thread_id: Option<String>,
    node_id: String,
    #[serde(default, skip_serializing_if = "is_false")]
    exclude_node: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery_run_id: Option<String>
});
args!(#[derive(Default)] ThreadSetArgs { #[serde(default, skip_serializing_if = "Option::is_none")] agents: Option<String>, #[serde(default)] connection: Option<String>, #[serde(default)] thread_id: Option<String>, #[serde(default)] cwd: Option<String>, #[serde(default)] model: Option<String>, #[serde(default)] effort: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] title: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] auto_compact_at: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] auto_compact_target_percent: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] summary_max_chars: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] max_tokens: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] spill: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] parent_thread_id: Option<String> });
impl ThreadSetArgs {
    pub fn summary_settings(
        thread_id: String,
        target_percent: Option<String>,
        summary_max_chars: Option<String>,
    ) -> Self {
        Self {
            thread_id: Some(thread_id),
            auto_compact_target_percent: target_percent,
            summary_max_chars,
            ..Default::default()
        }
    }
}
args!(MessageListArgs { #[serde(default)] owed_by_thread_id: Option<String>, #[serde(default)] asked_by_thread_id: Option<String>, #[serde(default)] to: Option<String>, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String> });
args!(MessageAskArgs {
    #[serde(default, skip_serializing_if = "is_false")] human: bool,
    /// Self-asserted sender name; at most 64 Unicode characters, without controls or line separators.
    #[serde(default, skip_serializing_if = "Option::is_none")] sender_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] to: Option<String>,
    #[serde(default, skip_serializing_if = "Deliver::is_next")] #[schemars(with = "NewDelivery")] deliver: Deliver,
    text: String,
});
args!(MessageSendArgs { #[serde(default, skip_serializing_if = "Option::is_none")] max_model_requests: Option<std::num::NonZeroU32>, #[serde(default, skip_serializing_if = "is_false")] human: bool, #[serde(default, skip_serializing_if = "is_false")] resume_recovery: bool, #[serde(default, skip_serializing_if = "Option::is_none")] sender_label: Option<String>, #[serde(default)] thread_id: Option<String>, text: String, #[serde(default, skip_serializing_if = "Deliver::is_next")] #[schemars(with = "NewDelivery")] deliver: Deliver });
/// Message delivery timing; `Complete` is Revisit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Deliver {
    #[default]
    Next,
    Now,
    Complete,
}
// Decode legacy arguments unchanged for exact persisted replay. The public
// schema advertises only values admitted for fresh operations.
#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(rename_all = "snake_case")]
enum NewDelivery {
    Next,
    Complete,
}

impl Deliver {
    pub(crate) fn validate_new(self) -> Result<(), ApiError> {
        if self == Self::Now {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "deliver now has been removed",
            ));
        }
        Ok(())
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Now => "now",
            Self::Complete => "complete",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Next, Self::Now, Self::Complete]
            .into_iter()
            .find(|deliver| deliver.as_str() == value)
    }
    pub fn is_next(&self) -> bool {
        *self == Self::Next
    }
}
args!(MessageReplyArgs {
    message_id: String,
    text: String,
    /// Accepted for existing callers; does not override the saved addressee.
    #[serde(default, skip_serializing_if = "Option::is_none")] sender_label: Option<String>,
});
args!(MessageCancelArgs { message_ids: Vec<String> });
states!(MessageCancelOutcome {
    ReplyCancelled,
    Cancelled,
    AlreadyCancelled,
    AlreadyAdmitted,
    NotFound,
    Forbidden,
    Unavailable
});
record!(MessageCancelItem { message_id: String, outcome: MessageCancelOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")] thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] archive_draft_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] archive_order: Option<u32>
});
record!(MessageCancelResult { items: Vec<MessageCancelItem> });
states!(IdKind {
    Thread,
    Message,
    Run,
    ShellExecution
});
args!(IdResolveArgs {
    kind: IdKind,
    prefix: String,
    #[serde(default, skip_serializing_if = "is_false")]
    include_pending_duties: bool
});
record!(IdResolveResult { id: String });
args!(MessageTarget {
    message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_thread_id: Option<String>,
});
record!(MessageSource {
    message_id: String,
    thread_id: String,
    selection_revision: u64,
    text: String,
    attachments: Vec<crate::draft::UiAttachment>,
});
args!(OperationTarget {
    operation_id: String
});
args!(RunTarget { run_id: String });
args!(ExpectedTurn {
    turn_id: String,
    generation: u64
});
args!(ExpectedRunSelection {
    thread_id: String,
    turn: Option<ExpectedTurn>,
});
args!(RunRetryArgs {
    run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_turn: Option<ExpectedTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_selection: Option<ExpectedRunSelection>,
});
impl RunRetryArgs {
    pub fn exact(run_id: String) -> Self {
        Self {
            run_id,
            expected_turn: None,
            expected_selection: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum RunStopTarget {
    Run {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_selection: Option<ExpectedRunSelection>,
    },
    Thread {
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_turn: Option<ExpectedTurn>,
    },
}
impl RunStopTarget {
    pub fn exact(run_id: String) -> Self {
        Self::Run {
            run_id,
            expected_selection: None,
        }
    }
}
args!(TurnTarget { turn_id: String });
args!(ExecutionTarget {
    execution_id: String
});
fn list_limit() -> u32 {
    DEFAULT_LIST_LIMIT
}
fn history_excerpt_chars() -> u32 {
    DEFAULT_HISTORY_EXCERPT_CHARS
}
fn body_limit() -> u32 {
    MAX_BODY_BYTES
}
fn search_limit() -> u32 {
    DEFAULT_SEARCH_LIMIT
}
fn timeout_ms() -> u64 {
    DEFAULT_WAIT_TIMEOUT_MS
}
fn utf8() -> String {
    DEFAULT_ENCODING.to_owned()
}
/// Which Threads `thread list` returns with respect to archiving (#222).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArchivedFilter {
    #[default]
    Exclude,
    Include,
    Only,
}
impl ArchivedFilter {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exclude => "exclude",
            Self::Include => "include",
            Self::Only => "only",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Exclude, Self::Include, Self::Only]
            .into_iter()
            .find(|mode| mode.as_str() == value)
    }
    pub fn is_exclude(&self) -> bool {
        *self == Self::Exclude
    }
}
/// Ordering of the Thread list's unfiltered or archive-filtered rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadListOrder {
    #[default]
    Updated,
    Created,
}
impl ThreadListOrder {
    fn is_updated(&self) -> bool {
        *self == Self::Updated
    }
}
/// The current state reported by a Thread list row, not its activity phase.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadListState {
    Idle,
    Live,
    Stopping,
    Preparing,
    Running,
    Waiting,
    RecoveryWait,
    Completed,
    Stopped,
    Failed,
    AwaitingYou,
    ActiveWork,
    OtherWork,
}
impl ThreadListState {
    pub(crate) const ALL: &[Self] = &[
        Self::Idle,
        Self::Live,
        Self::Stopping,
        Self::Preparing,
        Self::Running,
        Self::Waiting,
        Self::RecoveryWait,
        Self::Completed,
        Self::Stopped,
        Self::Failed,
        Self::AwaitingYou,
        Self::ActiveWork,
        Self::OtherWork,
    ];
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Live => "live",
            Self::Stopping => "stopping",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::RecoveryWait => "recovery_wait",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::AwaitingYou => "awaiting_you",
            Self::ActiveWork => "active_work",
            Self::OtherWork => "other_work",
        }
    }
}
args!(ThreadListArgs { #[serde(default, skip_serializing_if = "Option::is_none")] root: Option<String>, #[serde(default, skip_serializing_if = "std::ops::Not::not")] include_descendant_count: bool, #[serde(default, skip_serializing_if = "std::ops::Not::not")] include_activity: bool, #[serde(default, skip_serializing_if = "std::ops::Not::not")] include_activity_evidence: bool, #[serde(default, skip_serializing_if = "Option::is_none")] state: Option<ThreadListState>, #[serde(default, skip_serializing_if = "Option::is_none")] thread_ids: Option<Vec<String>>, #[serde(default, skip_serializing_if = "ThreadListOrder::is_updated")] order: ThreadListOrder, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String>, #[serde(default)] query: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] awaiting: Option<String>, #[serde(default, skip_serializing_if = "ArchivedFilter::is_exclude")] archived: ArchivedFilter, #[serde(default, skip_serializing_if = "Vec::is_empty")] tags: Vec<String>, #[serde(default, skip_serializing_if = "Option::is_none")] cwd: Option<String> });
impl Default for ThreadListArgs {
    fn default() -> Self {
        Self {
            root: None,
            include_descendant_count: false,
            include_activity: false,
            include_activity_evidence: false,
            thread_ids: None,
            state: None,
            order: ThreadListOrder::Updated,
            limit: list_limit(),
            cursor: None,
            query: None,
            awaiting: None,
            archived: ArchivedFilter::Exclude,
            tags: Vec::new(),
            cwd: None,
        }
    }
}
args!(HistoryListArgs { #[serde(default = "history_excerpt_chars")] excerpt_chars: u32, thread_id: String, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] shell_state: Option<String>
});
fn window_side() -> u32 {
    20
}
// Positional read of the selected path: no anchor means its end. `before` and
// `after` count items around the anchor; `before + after + 1` is capped at
// `MAX_LIST_LIMIT`. `expect_node_id` pairs with `ordinal` to detect a path
// that was rewound or compacted since the caller last saw it.
// `observe_node_ids` (with `include_observation`, at most 128) adds the
// mutable tool facts of other saved nodes, read in the same observation.
args!(HistoryWindowArgs { thread_id: String,
    #[serde(default, skip_serializing_if = "is_false")] include_observation: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] observe_node_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")] items: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] ordinal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] expect_node_id: Option<String>,
    #[serde(default = "window_side")] before: u32,
    #[serde(default = "window_side")] after: u32,
    #[serde(default = "history_excerpt_chars")] excerpt_chars: u32
});
// Items carry their own `selection.ordinal`. `total` is one past the last
// ordinal; `revision` is the Thread's selected-path revision (it also advances
// on append, which keeps every earlier position valid).
record!(HistoryWindow { thread_id: String, revision: u64, total: u64, items: Vec<HistorySnippet>,
    #[serde(default, skip_serializing_if = "Option::is_none")] observation: Option<HistoryWindowObservation>
});
record!(HistoryWindowObservation {
    read_sequence: u64,
    observed_at: String,
    pending: HistoryWindowPending,
    committed_attempts: Vec<HistoryWindowCommittedAttempt>,
    activity: HistoryWindowActivity,
    tools: HistoryWindowTools,
    recovery: Vec<HistoryWindowRecovery>,
    usage: Vec<HistoryWindowUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pinned_sources: Vec<HistoryWindowSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    received: Vec<HistoryWindowReceived>,
    received_root_revision: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    branches: Vec<HistoryWindowBranch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    compactions: Vec<HistoryWindowCompaction>,
});
record!(HistoryWindowCompaction {
    node_id: String,
    facts: crate::presentation::compaction_marker::MarkerFacts,
});
record!(HistoryWindowBranch {
    anchor_node_id: String,
    current: usize,
    alternatives: Vec<HistoryWindowBranchAlternative>,
});
record!(HistoryWindowBranchAlternative {
    thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    excerpt: Option<String>,
});
record!(HistoryWindowReceived {
    source_revision: i64,
    node_id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")] after_node_id: Option<String>,
    recovery_wait: bool
});
record!(HistoryWindowSource {
    node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")] created_at: Option<String>,
    relationship: crate::presentation::InputSource,
    sender: MessageSender,
    #[serde(skip_serializing_if = "Option::is_none")] reply_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] reply_excerpt: Option<String>
});
record!(HistoryWindowTools {
    calls: Vec<HistoryWindowToolCall>,
    limited_node_ids: Vec<String>,
    notices: Vec<HistoryWindowNotice>
});
// A saved `shell_ended` notification node joined to its exact execution, call
// and result. `source_node_id` is absent when the result is missing or reclaimed.
record!(HistoryWindowNotice {
    node_id: String,
    execution_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")] execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] background: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")] description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] source_node_id: Option<String>
});
record!(HistoryWindowToolCall {
    node_id: String, group_id: String, call_id: String, ordinal: u32,
    result_state: String, released: bool, background: bool,
    #[serde(skip_serializing_if = "Option::is_none")] command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] shell_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] shell_exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")] execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] shell_owner: Option<ShellReadOwner>,
    #[serde(skip_serializing_if = "Option::is_none")] call_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] result_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] prior_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] output_revision: Option<(u64, bool)>,
    #[serde(default, skip_serializing_if = "Option::is_none")] retention_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")] has_output: bool
});
record!(HistoryWindowRecovery {
    node_id: String, failure_count: u64, latest_attempt_id: String,
    #[serde(skip_serializing_if = "Option::is_none")] source_node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] reason: Option<HistoryWindowRecoveryReason>
});
record!(HistoryWindowRecoveryReason {
    kind: String,
    excerpt: String,
    truncated: bool
});
record!(HistoryWindowUsage { node_id: String, sources: Vec<HistoryWindowUsageSource> });
record!(HistoryWindowUsageSource {
    attempt_id: String,
    #[serde(skip_serializing_if = "Option::is_none")] input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] cached_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] input_bytes: Option<HistoryWindowInputBytes>
});
record!(HistoryWindowInputBytes { total: u64, categories: std::collections::BTreeMap<String, u64> });
record!(HistoryWindowPending { items: Vec<HistoryWindowPendingMessage>, total: u64, total_exact: bool });
record!(HistoryWindowPendingMessage {
    message_id: String, operation_id: String, delivery_status: DeliveryStatus,
    sender: MessageSender,
    expect_reply: bool,
    admission_uncertain: bool,
    completion_scheduled: bool,
    created_at: String,
    excerpt: String,
    excerpt_truncated: bool,
    attachment_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")] held_by: Option<MessageHeldBy>
});
record!(HistoryWindowCommittedAttempt {
    attempt_id: String,
    node_id: String
});
record!(#[derive(Default)] HistoryWindowActivity {
    #[serde(skip_serializing_if = "Option::is_none")] run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] waiting_reason: Option<RunWaitReason>
});
args!(HistoryReceivedArgs { thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] cursor: Option<String>
});
record!(HistoryReceivedPage { items: Vec<HistoryReceivedRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")] next_cursor: Option<String>
});
record!(HistoryReceivedRow { node_id: String, turn_id: String, kind: String, at: String,
    state: String, request: Option<String>, request_limited: bool, excerpt: Option<String>, excerpt_limited: bool
});
args!(HistoryRecallArgs { thread_id: String, older: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] anchor: Option<HistoryRecallAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")] expected_revision: Option<u64>
});
record!(HistoryRecallAnchor {
    ordinal: u64,
    node_id: String
});
record!(HistoryRecalledInput {
    ordinal: u64,
    node_id: String,
    text: String
});
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HistoryRecallResult {
    Found {
        input: HistoryRecalledInput,
    },
    NotInWindow {
        boundary: HistoryRecallAnchor,
        revision: u64,
    },
    End,
}
fn is_false(value: &bool) -> bool {
    !*value
}
fn is_true(value: &bool) -> bool {
    *value
}
fn default_true() -> bool {
    true
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HistoryDisplayPosition {
    Tail,
    Absolute { offset: u64 },
    FromEnd { offset: u64 },
}
args!(HistoryDisplaySelection { thread_id: String });
args!(HistoryDisplayRead {
    position: HistoryDisplayPosition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selection: Option<HistoryDisplaySelection>
});
args!(HistoryReadArgs { #[serde(default, skip_serializing_if = "Option::is_none")] display: Option<HistoryDisplayRead>, #[serde(default, skip_serializing_if = "is_false")] end_message_only: bool, #[serde(default, skip_serializing_if = "is_false")] parts: bool, node_id: String, #[serde(default = "body_limit")] limit_bytes: u32, #[serde(default)] cursor: Option<String>, #[serde(default = "utf8")] encoding: String });
args!(HistorySearchArgs {
    query: String,
    #[serde(default = "search_limit")]
    limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    exclude_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<String>
});
impl Default for HistorySearchArgs {
    fn default() -> Self {
        Self {
            query: String::new(),
            limit: search_limit(),
            thread_id: None,
            kinds: Vec::new(),
            exclude_kinds: Vec::new(),
            from: None,
            to: None,
        }
    }
}
states!(HistoryRelation {
    Previous => "previous", Next => "next", Sources => "sources", Replacements => "replacements"
});
args!(HistoryExportArgs {thread_id: String, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String> });
record!(HistoryExportPage {items: Vec<HistoryExportRecord>, next_cursor: Option<String>, change_position: u64});
record!(HistoryExportNode {id: String, order: i64, current_ordinal: Option<i64>, #[serde(skip_serializing_if = "Option::is_none")] payload_json: Option<String>, #[serde(default, skip_serializing_if = "Vec::is_empty")] import_origins: Vec<ImportOrigin>, provenance: NodeProvenance});
record!(ImportOrigin {
    source_thread_id: String,
    source_node_id: String,
    source_provenance: NodeProvenance,
    source_graph: HistoryExportRelations
});
record!(HistoryExportRelations {previous: Vec<HistoryExportReference>, next: Vec<HistoryExportReference>, replacement_groups: Vec<HistoryExportGroup>, replaced_by: Vec<HistoryExportMembership>});
record!(HistoryExportReference {
    id: String,
    scope: HistoryExportReferenceScope
});
states!(HistoryExportReferenceScope { Internal => "internal", External => "external", Missing => "missing" });
record!(HistoryExportGroup {id: String, state: String, sources: Vec<HistoryExportSource>});
record!(HistoryExportSource {
    ordinal: i64,
    node: HistoryExportReference
});
record!(HistoryExportMembership {
    group_id: String,
    ordinal: i64,
    output: HistoryExportReference
});
states!(HistoryExportRole { Current => "current", ReplacedSource => "replaced_source" });
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HistoryExportRecord {
    Node {
        role: HistoryExportRole,
        node: Box<HistoryExportNode>,
        relations: HistoryExportRelations,
        owner_thread_id: Option<String>,
    },
    Tombstone {
        role: HistoryExportRole,
        node: Box<HistoryExportNode>,
        relations: HistoryExportRelations,
        owner_thread_id: Option<String>,
        reason: String,
    },
}
args!(HistoryRelationsArgs { node_id: String, relation: HistoryRelation, #[serde(default = "list_limit")] limit: u32, #[serde(default, skip_serializing_if = "Option::is_none")] cursor: Option<String> });
states!(MessageReplyState { Pending => "pending", Replied => "replied", Ignored => "ignored", Cancelled => "cancelled" });
record!(MessageProvenance {
    expect_reply: bool,
    #[serde(default, skip_serializing_if="Option::is_none")] reply_to: Option<String>,
    #[serde(default, skip_serializing_if="Option::is_none")] reply_state: Option<MessageReplyState>,
    #[serde(default, skip_serializing_if="Option::is_none")] reply_node_id: Option<String>
});
record!(NodeProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")] output_limit: Option<AttemptOutputLimit>,
    #[serde(default, skip_serializing_if="Option::is_none")] notification: Option<NotificationDescriptor>,
    #[serde(default, skip_serializing_if="Option::is_none")] message: Option<MessageProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")] sender: Option<MessageSender>,
    /// Operation that created this user node, when uniquely proven by its receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")] client_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] attempt_id: Option<String>,
    kind: String,
    created_at: String,
    source_revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")] turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] origin_thread_id: Option<String>
});
record!(RelationItem { node_id: String, provenance: NodeProvenance, #[serde(default, skip_serializing_if = "Option::is_none")] replacement_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] source_revision: Option<i64>, #[serde(default, skip_serializing_if = "Option::is_none")] source_hash: Option<String> });
args!(ShellReadOwner {
    thread_id: String,
    run_id: String
});
args!(ShellReadArgs { execution_id: String, #[serde(default, skip_serializing_if="Option::is_none")] expected_owner: Option<ShellReadOwner>, #[serde(default, skip_serializing_if="is_false")] tail: bool, #[serde(default, skip_serializing_if="is_false")] segments: bool, #[serde(default)] cursor: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] encoding: Option<String>, #[serde(default = "body_limit")] limit_bytes: u32 });
args!(ShellWriteArgs {
    execution_id: String,
    text: String,
    #[serde(default = "utf8")]
    encoding: String
});
args!(AttachmentAddArgs { #[serde(default)] thread_id: Option<String>, path: String, kind: AttachmentKind });
args!(AttachmentRemoveArgs { #[serde(default)] thread_id: Option<String>, attachment_id: String });
args!(#[derive(Default)] IndexArgs { #[serde(default, skip_serializing_if = "Option::is_none")] dimensions: Option<usize>, #[serde(default)] scope: Option<IndexScope>, #[serde(default)] space_id: Option<String>, #[serde(default)] thread_ids: Vec<String>, #[serde(default)] source_ids: Vec<String>, #[serde(default, skip_serializing_if = "Vec::is_empty")] kinds: Vec<String> });
args!(#[derive(Default)] IndexStatusArgs { #[serde(default)] scope: Option<IndexScope>, #[serde(default)] space_id: Option<String>, #[serde(default)] thread_ids: Vec<String>, #[serde(default)] source_ids: Vec<String>, #[serde(default, skip_serializing_if = "is_false")] #[schemars(description = "Also report `coverage_window`: one bounded window of per-source coverage for the selected scope.")] coverage: bool, #[serde(default, skip_serializing_if = "Option::is_none")] #[schemars(description = "`coverage_window.next_cursor` of the previous window; requires `coverage` and the same selection.")] coverage_cursor: Option<String> });
impl IndexStatusArgs {
    /// The index selection the coverage window is scoped to.
    pub fn selection(&self) -> IndexArgs {
        IndexArgs {
            dimensions: None,
            scope: self.scope,
            space_id: self.space_id.clone(),
            thread_ids: self.thread_ids.clone(),
            source_ids: self.source_ids.clone(),
            kinds: Vec::new(),
        }
    }
}
impl From<IndexArgs> for IndexStatusArgs {
    fn from(value: IndexArgs) -> Self {
        Self {
            scope: value.scope,
            space_id: value.space_id,
            thread_ids: value.thread_ids,
            source_ids: value.source_ids,
            coverage: false,
            coverage_cursor: None,
        }
    }
}
args!(StorageArgs { #[serde(default)] category: Option<StorageCategory> });
args!(StatusArgs { #[serde(default)] memory: bool, #[serde(default)] thread_id: Option<String>, #[serde(default)] run_id: Option<String>, #[serde(default)] since: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] ended_after: Option<String>, #[serde(default)] state: Vec<String>, #[serde(default)] all: bool, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String> });
impl Default for StatusArgs {
    fn default() -> Self {
        Self {
            memory: false,
            thread_id: None,
            run_id: None,
            since: None,
            ended_after: None,
            state: Vec::new(),
            all: false,
            limit: DEFAULT_LIST_LIMIT,
            cursor: None,
        }
    }
}
// Receipt-free long-poll over the owner's in-memory change clock (#243).
args!(ConfigGetArgs {});
args!(ConfigSetArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_hash: Option<String>,
    changes: Vec<ConfigChange>
});
/// One named change to the configuration file. `field` is a TOML dotted key;
/// a missing or null `value` removes the key so its default applies again.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigChange {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}
/// A value may be a header or environment value, so Debug shows only its name.
impl std::fmt::Debug for ConfigChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigChange")
            .field("field", &self.field)
            .field("value", &self.value.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}
/// The key is write-only: Debug and every reply leave it out.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialsSetArgs {
    pub connection: String,
    pub value: String,
}
impl std::fmt::Debug for CredentialsSetArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialsSetArgs")
            .field("connection", &self.connection)
            .field("value", &"[redacted]")
            .finish()
    }
}
args!(CredentialsClearArgs { connection: String });
/// Test a saved connection by `connection`, or a candidate that is not saved by
/// `base_url` (with optional `api`, `api_key` or `api_key_env`, `api_key_header`,
/// `ssl_verify`). Exactly one of the two forms.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionTestArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<crate::config::Api>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssl_verify: Option<bool>,
    /// Opt in to one paid synthetic conversation POST after the catalog GET.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_model: Option<String>,
}
impl std::fmt::Debug for ConnectionTestArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionTestArgs")
            .field("connection", &self.connection)
            .field("base_url", &self.base_url.as_ref().map(|_| "[redacted]"))
            .field("api", &self.api)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("api_key_env", &self.api_key_env)
            .field("api_key_header", &self.api_key_header)
            .field("ssl_verify", &self.ssl_verify)
            .field("post_model", &self.post_model)
            .finish()
    }
}
args!(EventsWaitArgs { #[serde(default, skip_serializing_if="Option::is_none")] after: Option<String>, #[serde(default, skip_serializing_if="Vec::is_empty")] thread_ids: Vec<String>, #[serde(default = "timeout_ms")] timeout_ms: u64, #[serde(default, skip_serializing_if="std::ops::Not::not")] include_text: bool, #[serde(default, skip_serializing_if="std::ops::Not::not")] include_reasoning_activity: bool, #[serde(default, skip_serializing_if="std::ops::Not::not")] include_output: bool });
record!(EventChange {
    thread_id: String,
    seq: u64
});
// Committed shell output after the cursor: the execution's latest output position only.
// `output_at` is that batch's saved receipt time (`shell.get` `last_output_at`, RFC 3339).
// Read the output itself with `shell.read` or `wait`.
record!(EventOutput {
    thread_id: String,
    execution_id: String,
    output_at: String,
    seq: u64
});
record!(EventRecoveryTransition {
    thread_id: String,
    seq: u64
});
// Unsaved answer text (#243 E3). `offset` counts Unicode scalar values in the attempt's
// full model stream, including omitted reasoning deltas; it restarts with each attempt.
record!(EventTextDelta {
    offset: u64,
    reasoning: bool,
    text: String
});
record!(EventLiveText {
    thread_id: String,
    attempt_id: String,
    input_through_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receive_age_ms: Option<u64>,
    gap: bool,
    deltas: Vec<EventTextDelta>
});
record!(EventReasoningActivity {
    thread_id: String,
    attempt_id: String,
    elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    received_bytes: Option<u64>
});
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventAttemptOutcome {
    Adopted,
    NotAdopted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputLimitSource {
    ThreadOverride,
    Connection,
    Default,
    ExtraBody,
}
record!(AttemptOutputLimit {
    stop: bool,
    sent_max_tokens: Option<u64>,
    source: Option<OutputLimitSource>,
    connection: String
});
record!(EventAttemptEnd {
    thread_id: String,
    attempt_id: String,
    outcome: EventAttemptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")] output_limit: Option<AttemptOutputLimit>,
    seq: u64
});
record!(EventsWaitResult { status: EventsWaitStatus, cursor: String, changes: Vec<EventChange>, more: bool, config_changed: bool, index_changed: bool, retention_changed: bool, #[serde(default, skip_serializing_if="Vec::is_empty")] recovery_transitions: Vec<EventRecoveryTransition>, #[serde(default, skip_serializing_if="Option::is_none")] live_text: Option<Vec<EventLiveText>>, #[serde(default, skip_serializing_if="Option::is_none")] attempt_ends: Option<Vec<EventAttemptEnd>>, #[serde(default, skip_serializing_if="Option::is_none")] output: Option<Vec<EventOutput>>, #[serde(default)] reasoning_activity: Vec<EventReasoningActivity> });
pub const MAX_EVENT_THREAD_FILTER: usize = 64;
pub const MAX_EVENT_CHANGES: usize = 64;
/// Opaque cursor: the owner instance that issued it and a position of that instance's change clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventCursor {
    pub v: u32,
    pub instance_id: String,
    pub seq: u64,
}
impl EventCursor {
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("event cursor serializes"))
    }
    pub fn decode(token: &str) -> Result<Self, ApiError> {
        let bad = || invalid("events cursor is malformed; restart without --after");
        if token.len() > 1024 {
            return Err(bad());
        }
        let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| bad())?;
        let value: Self = serde_json::from_slice(&bytes).map_err(|_| bad())?;
        if value.v != 1 || value.instance_id.is_empty() {
            return Err(bad());
        }
        Ok(value)
    }
}
args!(WaitArgs { conditions: Vec<WaitCondition>, #[serde(default = "timeout_ms")] timeout_ms: u64, #[serde(default, skip_serializing_if="Option::is_none")] after: Option<String> });

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WaitCondition {
    MessageDelivered {
        message_id: String,
    },
    ReplyReceived {
        message_id: String,
    },
    ThreadSettled {
        thread_ids: Vec<String>,
        states: Vec<String>,
    },
    TurnCompleted {
        turn_id: String,
    },
    RunEnded {
        run_id: String,
    },
    ShellEnded {
        execution_id: String,
    },
    ShellOutput {
        execution_id: String,
        cursor: String,
    },
}

args!(DraftSubmitArgs {
    thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_thread_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_source_message: Option<String>,
    create: ThreadCreateArgs,
    draft: Box<crate::draft::DraftSnapshot>,
    #[serde(default)] #[schemars(with = "NewDelivery")] deliver: Deliver,
});
pub const DRAFT_IMAGE_CHUNK_BYTES: usize = 1024 * 1024;

args!(DraftImageArgs {
    instance_id: String,
    draft_id: String,
    action: DraftImageAction
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftImageAction {
    Begin {
        encoded_bytes: u64,
    },
    Append {
        upload_id: String,
        offset: u64,
        base64: String,
    },
    Commit {
        upload_id: String,
    },
    Release {
        attachment_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DraftImageResult {
    Begun { upload_id: String },
    Appended { received_bytes: u64 },
    Committed { attachment: DraftImageAttachment },
    Released,
}

record!(DraftImageAttachment {
    id: String,
    path: std::path::PathBuf
});

args!(DraftGetArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")] thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] operation_id: Option<String>,
});
args!(DraftListArgs {
    #[serde(default)] thread_id: Option<String>,
    #[serde(default)] kind: DraftListKind,
    #[serde(default = "list_limit")] limit: u32,
    #[serde(default)] cursor: Option<String>,
});
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum DraftListKind {
    #[default]
    Restorable,
    Submitted,
}
record!(DraftListFilters { thread_id: Option<String>, kind: DraftListKind });
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftRecord {
    Cancelled {
        draft_id: String,
        source_message_id: Option<String>,
    },
    Unconfirmed {
        draft_id: String,
        excerpt: String,
    },
    Submitted {
        draft_id: String,
        thread_id: String,
        operation_id: String,
        source_draft_id: String,
        source_revision: u64,
        message_id: String,
    },
}
pub type DraftListPage = RecordPage<DraftRecord, DraftListFilters>;
args!(DraftRestoreArgs { archive_id: String });
record!(DraftState {
    selection_revision: u64,
    draft: Option<crate::draft::DraftSnapshot>,
    view: Option<DraftView>,
    #[serde(default, skip_serializing_if = "Option::is_none")] record: Option<DraftRecord>,
});
record!(DraftView {
    anchor: crate::draft::ViewAnchor,
    follow_tail: bool
});
