//! Versioned local-operation contracts shared by CLI, Control, and TUI.
mod sender;
pub use sender::{MessageSender, SenderKind, validate_sender_label};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_REVISIT_TEXT_BYTES: usize = 8192;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_LIST_LIMIT: u32 = 50;
pub const MAX_LIST_LIMIT: u32 = 200;
pub const DEFAULT_HISTORY_EXCERPT_CHARS: u32 = 512;
pub const MAX_HISTORY_EXCERPT_CHARS: u32 = 4096;
pub const MAX_BODY_BYTES: u32 = 64 * 1024;
pub const DEFAULT_SEARCH_LIMIT: u32 = 10;
pub const MAX_SEARCH_LIMIT: u32 = 50;
pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_ENCODING: &str = "UTF-8";

macro_rules! states {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}
states!(ErrorCode {
    InvalidArgument,
    ConfigInvalid,
    AppNotRunning,
    ProtocolMismatch,
    ThreadNotFound,
    TargetUnavailable,
    OperationConflict,
    OperationExpired,
    OperationNotFound,
    InvalidState,
    RevisitLimitReached,
    WaitCycle,
    CursorExpired,
    CursorEncodingMismatch,
    OutputExpired,
    InputLimit,
    StorageLimit,
    SaveFailed,
    IoFailed
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
states!(AttachmentKind { Image => "image", File => "file" });
states!(StorageCategory {
    Raw => "raw", ManagedImages => "managed_images", History => "history", Index => "index", All => "all"
});
states!(IndexScope { Threads => "threads", All => "all" });

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub details: Option<Value>,
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
        }
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
            ErrorCode::ThreadNotFound | ErrorCode::TargetUnavailable => "Check the returned target ID and --data-dir with the corresponding get command; do not infer a replacement ID",
            ErrorCode::OperationConflict => "Query the existing receipt with shellboy operation get --operation OPERATION_ID; do not change the ID to repeat the same side effect",
            ErrorCode::OperationExpired | ErrorCode::OperationNotFound => "Inspect saved target state and history; an unavailable receipt does not prove the mutation was never applied",
            ErrorCode::InvalidState => "Inspect the target with its get command; read shellboy docs get --id shellboy/recovery-and-storage before choosing retry or stop",
            ErrorCode::RevisitLimitReached => "Inspect revisit get and the configured per-Run limit; continue or stop this Run explicitly rather than retrying registration",
            ErrorCode::WaitCycle => "Inspect Run ownership and wait dependencies; correct the wait direction instead of polling around the cycle",
            ErrorCode::CursorExpired => "Restart the same list/read with its original target and filters, without the old cursor; previously returned items or bytes may repeat",
            ErrorCode::CursorEncodingMismatch => "Restart shell read without the old cursor and specify the desired encoding; previously read bytes may be returned again",
            ErrorCode::OutputExpired => "Inspect shell state and retained history; unavailable raw output cannot be reconstructed by retrying read",
            ErrorCode::InputLimit => "Inspect the input bound in command help; do not treat truncated input as accepted",
            ErrorCode::StorageLimit => "Inspect shellboy storage status and storage capacity; resolve the cause before explicitly choosing a recovery action",
            ErrorCode::SaveFailed => "Preserve unsaved input and inspect capacity/permissions; do not keep submitting mutations while saving is unavailable",
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OriginIdentity<'a> {
    pub instance_id: &'a str,
    pub thread_id: &'a str,
    pub run_id: &'a str,
    pub turn_id: &'a str,
    pub execution_id: &'a str,
}
impl Origin {
    pub fn identity(&self) -> OriginIdentity<'_> {
        OriginIdentity {
            instance_id: &self.instance_id,
            thread_id: &self.thread_id,
            run_id: &self.run_id,
            turn_id: &self.turn_id,
            execution_id: &self.execution_id,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
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
fn request_fields(value: &Value) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("request must be an object")?;
    const FIELDS: &[&str] = &[
        "protocol_version",
        "operation_id",
        "command",
        "args",
        "origin",
    ];
    if object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
        return Err("unknown request field");
    }
    Ok(())
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
    /// Strict top-level parsing is explicit because serde flatten cannot deny unknown fields.
    pub fn from_value(value: Value) -> Result<Self, ApiError> {
        request_fields(&value).map_err(invalid)?;
        let request: Self =
            serde_json::from_value(value).map_err(|_| invalid("invalid request shape"))?;
        request.validate()?;
        Ok(request)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($(#[$meta])* pub $field: $ty),* }
    };
}
args!(ThreadTarget { #[serde(default)] thread_id: Option<String> });
args!(#[derive(Default)] UsageGetArgs { #[serde(default)] since: Option<String>, #[serde(default)] until: Option<String>, #[serde(default)] thread_id: Option<String>, #[serde(default)] run_id: Option<String>, #[serde(default)] turn_id: Option<String>, #[serde(default)] include_children: bool });
fn readback_default_limit() -> u32 {
    50
}

args!(CompactListArgs { #[serde(default)] thread_id:Option<String>, #[serde(default)] since:Option<String>, #[serde(default)] until:Option<String>, #[serde(default="readback_default_limit")] limit:u32 });
args!(ShellListArgs { #[serde(default)] thread_id:Option<String>, #[serde(default)] since:Option<String>, #[serde(default)] until:Option<String>, #[serde(default)] command_contains:Option<String>, #[serde(default)] state:Option<String>, #[serde(default)] exit_code:Option<i32>, #[serde(default="readback_default_limit")] limit:u32, #[serde(default)] cursor:Option<String> });
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
    #[serde(default)] max_chars: Option<u64>,
    #[serde(default)] max_percent: Option<f64>
});
args!(ModelListArgs { #[serde(default)] connection: Option<String>, #[serde(default)] thread_id: Option<String> });
args!(ModelListResult { connection: String, models: Vec<String>, api: String, unavailable_features: Vec<String> });
args!(ThreadCreateArgs { #[serde(default, skip_serializing_if = "Option::is_none")] agents: Option<String>, #[serde(default)] connection: Option<String>, #[serde(default)] cwd: Option<String>, #[serde(default)] model: Option<String>, #[serde(default)] effort: Option<String>, #[serde(default)] parent_id: Option<String> });
args!(ThreadForkArgs { #[serde(default, skip_serializing_if = "Option::is_none")] agents: Option<String>, thread_id: String, node_id: String, #[serde(default)] cwd: Option<String> });
args!(ThreadDeleteArgs {
    thread_id: String,
    #[serde(default)]
    descendants: bool
});
args!(#[derive(Default)] ThreadSetArgs { #[serde(default, skip_serializing_if = "Option::is_none")] agents: Option<String>, #[serde(default)] connection: Option<String>, #[serde(default)] thread_id: Option<String>, #[serde(default)] cwd: Option<String>, #[serde(default)] model: Option<String>, #[serde(default)] effort: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] title: Option<String> });
args!(MessageSendArgs { #[serde(default, skip_serializing_if = "Option::is_none")] sender_label: Option<String>, #[serde(default)] thread_id: Option<String>, text: String, #[serde(default)] now: bool });
args!(MessageCancelArgs { message_ids: Vec<String> });
states!(MessageCancelOutcome {
    Cancelled,
    AlreadyCancelled,
    AlreadyAdmitted,
    NotFound,
    Forbidden,
    Unavailable
});
args!(MessageCancelItem { message_id: String, outcome: MessageCancelOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")] thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] archive_draft_id: Option<String>
});
args!(MessageCancelResult { items: Vec<MessageCancelItem> });
args!(MessageTarget { message_id: String });
args!(OperationTarget {
    operation_id: String
});
args!(RunTarget { run_id: String });
args!(RevisitArgs { run_id: Option<String> });
args!(RevisitSetArgs { run_id: Option<String>, text: String });
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
args!(ThreadListArgs { #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String>, #[serde(default)] query: Option<String> });
impl Default for ThreadListArgs {
    fn default() -> Self {
        Self {
            limit: list_limit(),
            cursor: None,
            query: None,
        }
    }
}
args!(HistoryListArgs { #[serde(default = "history_excerpt_chars")] excerpt_chars: u32, thread_id: String, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] state: Option<String>
});
fn is_false(value: &bool) -> bool {
    !*value
}
args!(HistoryGetArgs { #[serde(default, skip_serializing_if = "is_false")] final_answer_only: bool, node_id: String, #[serde(default = "body_limit")] limit_bytes: u32, #[serde(default)] cursor: Option<String>, #[serde(default = "utf8")] encoding: String });
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
args!(HistoryRelationsArgs { node_id: String, relation: HistoryRelation, #[serde(default = "list_limit")] limit: u32, #[serde(default, skip_serializing_if = "Option::is_none")] cursor: Option<String> });
args!(NodeProvenance { #[serde(default, skip_serializing_if = "Option::is_none")] sender: Option<MessageSender>, kind: String, created_at: String, source_revision: i64, #[serde(default, skip_serializing_if = "Option::is_none")] turn_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] run_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] origin_thread_id: Option<String> });
args!(RelationItem { node_id: String, provenance: NodeProvenance, #[serde(default, skip_serializing_if = "Option::is_none")] replacement_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] source_revision: Option<i64>, #[serde(default, skip_serializing_if = "Option::is_none")] source_hash: Option<String> });
args!(ShellReadArgs { execution_id: String, #[serde(default)] cursor: Option<String>, #[serde(default, skip_serializing_if="Option::is_none")] encoding: Option<String>, #[serde(default = "body_limit")] limit_bytes: u32 });
args!(ShellWriteArgs {
    execution_id: String,
    text: String,
    #[serde(default = "utf8")]
    encoding: String
});
args!(AttachmentAddArgs { #[serde(default)] thread_id: Option<String>, path: String, kind: AttachmentKind });
args!(AttachmentRemoveArgs { #[serde(default)] thread_id: Option<String>, attachment_id: String });
args!(#[derive(Default)] IndexArgs { #[serde(default, skip_serializing_if = "Option::is_none")] dimensions: Option<usize>, #[serde(default)] scope: Option<IndexScope>, #[serde(default)] space_id: Option<String>, #[serde(default)] thread_ids: Vec<String>, #[serde(default)] source_ids: Vec<String> });
args!(StorageArgs { #[serde(default)] category: Option<StorageCategory> });
args!(StatusArgs { #[serde(default)] thread_id: Option<String>, #[serde(default)] run_id: Option<String>, #[serde(default)] since: Option<String>, #[serde(default)] state: Vec<String>, #[serde(default)] all: bool, #[serde(default = "list_limit")] limit: u32, #[serde(default)] cursor: Option<String> });
impl Default for StatusArgs {
    fn default() -> Self {
        Self {
            thread_id: None,
            run_id: None,
            since: None,
            state: Vec::new(),
            all: false,
            limit: DEFAULT_LIST_LIMIT,
            cursor: None,
        }
    }
}
args!(WaitArgs { conditions: Vec<WaitCondition>, #[serde(default = "timeout_ms")] timeout_ms: u64 });

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WaitCondition {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", content = "args")]
pub enum Command {
    #[serde(rename = "status")]
    Status(StatusArgs),
    #[serde(rename = "model.list")]
    ModelList(ModelListArgs),
    #[serde(rename = "thread.list")]
    ThreadList(ThreadListArgs),
    #[serde(rename = "thread.get")]
    ThreadGet(ThreadTarget),
    #[serde(rename = "usage.get")]
    UsageGet(UsageGetArgs),
    #[serde(rename = "compact.list")]
    CompactList(CompactListArgs),
    #[serde(rename = "shell.list")]
    ShellList(ShellListArgs),
    #[serde(rename = "thread.create")]
    ThreadCreate(ThreadCreateArgs),
    #[serde(rename = "thread.fork")]
    ThreadFork(ThreadForkArgs),
    #[serde(rename = "thread.delete")]
    ThreadDelete(ThreadDeleteArgs),
    #[serde(rename = "thread.delete_preview")]
    ThreadDeletePreview(ThreadDeleteArgs),
    #[serde(rename = "thread.set")]
    ThreadSet(ThreadSetArgs),
    #[serde(rename = "message.send")]
    MessageSend(MessageSendArgs),
    #[serde(rename = "message.get")]
    MessageGet(MessageTarget),
    #[serde(rename = "message.cancel")]
    MessageCancel(MessageCancelArgs),
    #[serde(rename = "operation.get")]
    OperationGet(OperationTarget),
    #[serde(rename = "run.get")]
    RunGet(RunTarget),
    #[serde(rename = "run.retry")]
    RunRetry(RunTarget),
    #[serde(rename = "run.stop")]
    RunStop(RunTarget),
    #[serde(rename = "revisit.set")]
    RevisitSet(RevisitSetArgs),
    #[serde(rename = "revisit.get")]
    RevisitGet(RevisitArgs),
    #[serde(rename = "revisit.cancel")]
    RevisitCancel(RevisitArgs),
    #[serde(rename = "turn.get")]
    TurnGet(TurnTarget),
    #[serde(rename = "wait")]
    Wait(WaitArgs),
    #[serde(rename = "history.list")]
    HistoryList(HistoryListArgs),
    #[serde(rename = "history.get")]
    HistoryGet(HistoryGetArgs),
    #[serde(rename = "history.search")]
    HistorySearch(HistorySearchArgs),
    #[serde(rename = "history.relations")]
    HistoryRelations(HistoryRelationsArgs),
    #[serde(rename = "shell.get")]
    ShellGet(ExecutionTarget),
    #[serde(rename = "shell.read")]
    ShellRead(ShellReadArgs),
    #[serde(rename = "shell.write")]
    ShellWrite(ShellWriteArgs),
    #[serde(rename = "shell.eof")]
    ShellEof(ExecutionTarget),
    #[serde(rename = "shell.stop")]
    ShellStop(ExecutionTarget),
    #[serde(rename = "attachment.add")]
    AttachmentAdd(AttachmentAddArgs),
    #[serde(rename = "attachment.list")]
    AttachmentList(ThreadTarget),
    #[serde(rename = "attachment.remove")]
    AttachmentRemove(AttachmentRemoveArgs),
    #[serde(rename = "compact")]
    Compact(ThreadTarget),
    #[serde(rename = "context.edit")]
    ContextEdit(ContextEditArgs),
    #[serde(rename = "index.status")]
    IndexStatus(IndexArgs),
    #[serde(rename = "index.preview")]
    IndexPreview(IndexArgs),
    #[serde(rename = "index.backfill")]
    IndexBackfill(IndexArgs),
    #[serde(rename = "index.retry")]
    IndexRetry(IndexArgs),
    #[serde(rename = "index.rebuild")]
    IndexRebuild(IndexArgs),
    #[serde(rename = "index.probe")]
    IndexProbe(IndexProbeArgs),
    #[serde(rename = "storage.status")]
    StorageStatus(StorageArgs),
    #[serde(rename = "storage.collect")]
    StorageCollect(StorageArgs),
}
mod command;
pub(crate) use command::CommandRoute;

mod validation;
pub(crate) use validation::rfc3339;

args!(ShellToolArgs { command: String, #[serde(default)] background: bool, #[serde(default)] stdin_open: bool, #[serde(default)] initial_stdin: Option<String>, #[serde(default = "utf8")] input_encoding: String, #[serde(default = "utf8")] output_encoding: String });
impl ShellToolArgs {
    pub fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.command, "command")?;
        if self.command.contains('\0')
            || self.input_encoding.contains('\0')
            || self.output_encoding.contains('\0')
        {
            return Err(invalid(
                "shell command and encoding names cannot contain NUL",
            ));
        }
        nonempty(&self.input_encoding, "input_encoding")?;
        nonempty(&self.output_encoding, "output_encoding")
    }
}
args!(MessageAccepted { message_id: String, thread_id: String, delivery_status: DeliveryStatus, assignment: Assignment, #[serde(default)] now: bool, #[serde(default, skip_serializing_if = "Option::is_none")] run_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] turn_id: Option<String> });
impl MessageAccepted {
    pub fn validate(&self) -> Result<(), ApiError> {
        nonempty(&self.message_id, "message_id")?;
        nonempty(&self.thread_id, "thread_id")?;
        let assigned = self.assignment != Assignment::Pending;
        if assigned != (self.run_id.is_some() && self.turn_id.is_some())
            || self.run_id.is_some() != self.turn_id.is_some()
        {
            return Err(invalid("assignment and run/turn IDs disagree"));
        }
        optional_nonempty(self.run_id.as_deref(), "run_id")?;
        optional_nonempty(self.turn_id.as_deref(), "turn_id")
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
args!(WaitResult { status: WaitStatus, matched: Vec<WaitCondition>, snapshot: Value, #[serde(default, skip_serializing_if = "Option::is_none")] cursor: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] hint: Option<String> });

/// Versioned opaque page cursor. Scope, filters, and revision bind it to one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageCursor {
    pub version: u32,
    pub scope: String,
    pub filter: String,
    pub last_sort_key: String,
    pub watermark: u64,
    pub revision: u64,
    pub encoding: Option<String>,
}
impl PageCursor {
    pub fn encode(&self) -> Result<String, ApiError> {
        serde_json::to_vec(self)
            .map(|v| URL_SAFE_NO_PAD.encode(v))
            .map_err(|_| invalid("cursor cannot be encoded"))
    }
    pub fn decode(
        token: &str,
        scope: &str,
        filter: &str,
        revision: u64,
        encoding: Option<&str>,
    ) -> Result<Self, ApiError> {
        let expired = || {
            ApiError::new(
                ErrorCode::CursorExpired,
                "cursor no longer matches this query",
            )
        };
        if token.len() > 8192 {
            return Err(expired());
        }
        let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| expired())?;
        let value: Self = serde_json::from_slice(&bytes).map_err(|_| expired())?;
        if value.version != 1
            || value.scope != scope
            || value.filter != filter
            || value.revision != revision
        {
            return Err(expired());
        }
        if value.encoding.as_deref() != encoding {
            return Err(ApiError::new(
                ErrorCode::CursorEncodingMismatch,
                "cursor encoding changed",
            )
            .hint("restart reading without the previous cursor"));
        }
        Ok(value)
    }
}

/// Read one bounded frame. A clean EOF before the prefix is distinct from truncation.
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>, ApiError> {
    let mut prefix = [0; 4];
    let n = reader.read(&mut prefix[..1]).await.map_err(frame_io)?;
    if n == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut prefix[1..])
        .await
        .map_err(frame_io)?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "invalid or excessive frame length",
        ));
    }
    // Grow with received data, not an untrusted advertised allocation. Take
    // prevents read_to_end from consuming a following frame on this stream.
    let mut bytes = Vec::new();
    reader
        .take(len as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(frame_io)?;
    if bytes.len() != len {
        return Err(frame_io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "incomplete frame payload",
        )));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| invalid("invalid frame JSON"))
}
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), ApiError> {
    let bytes = encode_frame(value)?;
    write_encoded_frame(writer, &bytes).await
}

/// Prepare and validate the complete payload before any frame bytes are sent.
pub(crate) fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, ApiError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| invalid("value cannot be encoded as JSON"))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "frame exceeds the byte limit",
        ));
    }
    Ok(bytes)
}

pub(crate) async fn write_encoded_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> Result<(), ApiError> {
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(frame_io)?;
    writer.write_all(bytes).await.map_err(frame_io)?;
    writer.flush().await.map_err(frame_io)
}
fn frame_io(error: std::io::Error) -> ApiError {
    ApiError::new(
        ErrorCode::IoFailed,
        format!("local protocol I/O failed: {error}"),
    )
}

// Core result records deliberately retain provider-specific bodies as JSON payloads.
args!(RevisitSummary { pending: bool, consumed_count: u64, last_fired_at: Option<String> });
args!(ThreadSummary { activity: crate::activity::ActivityPhase, thread_id: String, title: String, cwd: String, #[serde(default, skip_serializing_if = "Option::is_none")] parent_id: Option<String>, state: String, latest_run_stop_reason: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] revisit: Option<RevisitSummary>, updated_at: String, excerpt: String });
args!(ThreadDetails { #[serde(default,skip_serializing_if="Option::is_none")] last_compaction:Option<Value>, #[serde(default, skip_serializing_if = "Option::is_none")] usage_totals: Option<Value>, agents_path: String, #[serde(default, skip_serializing_if = "Option::is_none")] context_usage: Option<Value>, #[serde(default, skip_serializing_if = "Option::is_none")] context: Option<crate::context::ExecutionContext>, thread_id: String, title: String, cwd: String, connection: String, model: String, #[serde(default, skip_serializing_if = "Option::is_none")] effort: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] parent_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] run_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] turn_id: Option<String>, children_count: u64, shells_count: u64, #[serde(default, skip_serializing_if = "Option::is_none")] recovery_reason: Option<String> });
args!(MessageDetails { sender: MessageSender, context: crate::context::ExecutionContext, message_id: String, thread_id: String, delivery_status: DeliveryStatus, assignment: Assignment, #[serde(default, skip_serializing_if = "Option::is_none")] run_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] turn_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] answer_node_id: Option<String> });
args!(RunDetails { #[serde(default, skip_serializing_if = "Option::is_none")] usage_totals: Option<Value>, context: crate::context::ExecutionContext, run_id: String, thread_id: String, state: RunState, #[serde(default, skip_serializing_if = "Option::is_none")] owner_run_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] turn_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] answer_node_id: Option<String>, owned_run_ids: Vec<String>, owned_execution_ids: Vec<String>, #[serde(default, skip_serializing_if = "Option::is_none")] recovery_hint: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] revisit: Option<RevisitSummary> });
args!(TurnDetails { #[serde(default, skip_serializing_if = "Option::is_none")] usage_totals: Option<Value>, context: crate::context::ExecutionContext, turn_id: String, run_id: String, thread_id: String, state: TurnState, #[serde(default, skip_serializing_if = "Option::is_none")] answer_node_id: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] recovery_hint: Option<String> });
// Optional shell fields describe unknown/live lifecycle facts and the internal
// output-reader snapshot, which deliberately does not load command ownership.
// They are not an older-owner protocol compatibility contract.
args!(ShellDetails {
    execution_id: String, run_id: String, state: ShellState, output_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] started_at: Option<String>,
    #[serde(default)] last_output_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] forced: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] exit_signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] group_cleanup_confirmed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")] elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] turn_id: Option<String>
});
args!(ShellOutput {
    execution_id: String,
    text: String,
    encoding: String,
    cursor: String,
    raw_bytes: u64,
    output_complete: bool,
    decode_errors: bool
});
args!(ShellWriteResult {
    execution_id: String,
    bytes_written: u64,
    state: ShellState
});
args!(HistoryExecutionLink {
    group_id: String, call_id: String, call_ordinal: u64, call_state: String,
    execution_id: String, state: String, cwd: String, command_excerpt: String,
    command_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] request_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] result_node_id: Option<String>
});
args!(HistorySelection {
    thread_id: String,
    ordinal: u64,
    current_cwd: String
});
args!(HistorySnippet {
    #[serde(default)] payload_chars: Option<u64>,
    node_id: String,
    kind: String,
    excerpt: String,
    provenance: NodeProvenance,
    context: crate::context::ExecutionContext,
    selection: HistorySelection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] executions: Vec<HistoryExecutionLink>,
    execution_count: u64,
    executions_truncated: bool
});
args!(HistoryBody { context: crate::context::ExecutionContext, provenance: NodeProvenance, node_id: String, kind: String, text: String, source_node_ids: Vec<String>, previous_node_ids: Vec<String>, next_node_ids: Vec<String>, #[serde(default, skip_serializing_if = "Option::is_none")] next_cursor: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] tombstone_reason: Option<String> });
args!(SearchMatch {
    node_id: String,
    excerpt: String,
    distance: f64
});
args!(AttachmentDetails { source_node_id: String, delivery_status: String, attachment_id: String, thread_id: String, path: String, kind: AttachmentKind, selected: bool, managed: bool, #[serde(default, skip_serializing_if = "Option::is_none")] unavailable_reason: Option<String> });
states!(IndexJobState {
    Pending,
    Running,
    Done,
    Failed
});
states!(ManagedFileState {
    Creating,
    Ready,
    Deleting,
    Deleted,
    Failed
});
args!(#[derive(Default)] IndexProbeArgs { #[serde(default, skip_serializing_if = "Option::is_none")] dimensions: Option<usize> });
args!(IndexStatus { #[serde(default)] auto_history: bool, #[serde(default)] held_auto_history: u64, #[serde(default, skip_serializing_if = "Option::is_none")] space_id: Option<String>, pending: u64, running: u64, done: u64, failed: u64, paused: bool, #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<String>, paused_at: Option<String>, pause_id: Option<String>, spaces: Vec<IndexSpaceStatus>, spaces_total: u64, spaces_truncated: bool, probes: Vec<IndexProbeObservation> });
args!(IndexSpaceStatus { space_id: String, connection: String, model: String, dimension: usize, requested_dimensions: Option<usize>, segments: u64, vector_payload_bytes: u64, state: String, selected: bool });
args!(IndexProbeObservation { connection: String, model: String, requested_dimensions: Option<usize>, dimension: Option<usize>, checked_at: String, success: bool, error: Option<Value> });
args!(StorageStatus { logical_live_bytes: u64, reclaimable_bytes: u64, db_bytes: u64, wal_bytes: u64, managed_bytes: u64, log_bytes: u64, #[serde(default, skip_serializing_if = "Option::is_none")] filesystem_free_bytes: Option<u64> });
args!(Accepted { accepted: bool });

#[cfg(test)]
mod message_acknowledgement_tests {
    use super::*;
    #[test]
    fn acknowledgement_delivery_mode_is_typed_defaults_false_and_keeps_unknown_field_rejection() {
        let mut wire = serde_json::json!({"message_id":"message","thread_id":"thread","delivery_status":"accepted","assignment":"new_run","run_id":"run","turn_id":"turn","now":true});
        let accepted: MessageAccepted = serde_json::from_value(wire.clone()).unwrap();
        accepted.validate().unwrap();
        assert!(accepted.now);
        assert_eq!(serde_json::to_value(&accepted).unwrap(), wire);
        wire.as_object_mut().unwrap().remove("now");
        assert!(
            !serde_json::from_value::<MessageAccepted>(wire.clone())
                .unwrap()
                .now
        );
        wire["unknown_delivery_flag"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MessageAccepted>(wire).is_err());
    }
}
