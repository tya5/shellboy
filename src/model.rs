//! Bounded Responses SSE parsing and completed-response validation.

pub(crate) mod cache_diagnostics;
pub mod content_filter;
pub mod dialect;
pub mod http;
pub mod opaque;
mod reconcile;
mod stream_error;
pub use stream_error::{StreamProviderStatus, StreamProviderStatusSource};
pub(crate) mod retry_class;
pub mod usage_limit;
pub mod wire_bytes;

use crate::protocol::ShellToolArgs;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

#[cfg(test)]
#[path = "model/output_limit_tests.rs"]
mod output_limit_tests;

pub const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ERROR_MESSAGE_CHARS: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMismatchField {
    Metadata,
    Type,
    Role,
    Content,
    Text,
    Refusal,
    CallId,
    Name,
    Arguments,
    ReasoningSummary,
    ReasoningContent,
    EncryptedContent,
    OutputIndex,
}
impl OutputMismatchField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Type => "type",
            Self::Role => "role",
            Self::Content => "content",
            Self::Text => "text",
            Self::Refusal => "refusal",
            Self::CallId => "call_id",
            Self::Name => "name",
            Self::Arguments => "arguments",
            Self::ReasoningSummary => "reasoning_summary",
            Self::ReasoningContent => "reasoning_content",
            Self::EncryptedContent => "encrypted_content",
            Self::OutputIndex => "output_index",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMismatch {
    pub output_index: u64,
    pub field: OutputMismatchField,
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct ModelError {
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub(crate) stream_provider_status: Option<StreamProviderStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_observation: Option<Box<http::HttpObservation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_limit: Option<Box<usage_limit::UsageLimit>>,
}
impl ModelError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            provider_code: None,
            status: None,
            http_observation: None,
            usage_limit: None,
            stream_provider_status: None,
        }
    }
    pub fn is_context_limit(&self) -> bool {
        matches!(
            self.provider_code.as_deref(),
            Some(
                "context_length_exceeded"
                    | "max_context_length_exceeded"
                    | "context_window_exceeded"
            )
        )
    }
    /// #182: the documented preserved-thinking rejection ("Invalid `signature`
    /// in `thinking` block. The block is bound to a different conversation.").
    /// Matched on the provider text only; no vendor-specific request knob is
    /// sent. A tampered signature lacks the "bound to" sentence and is not
    /// this case.
    pub fn is_thinking_binding(&self) -> bool {
        self.status.is_none_or(|status| status == 400)
            && self.message.contains("`thinking` block")
            && self.message.contains("bound to a different conversation")
    }
    /// Provider facts shared by embedding consumers; retry/pause budgets remain
    /// owned by the persistent index or the explicit RAG invocation.
    pub(crate) fn embedding_failure(&self) -> EmbeddingFailure {
        let credentials_or_credit = matches!(self.status, Some(401..=403));
        let invalid_configuration = matches!(
            self.kind.as_str(),
            "config_missing" | "config_missing_credentials" | "dimension_mismatch"
        );
        EmbeddingFailure {
            credentials_or_credit,
            invalid_configuration,
            input_limit: self.is_context_limit()
                || matches!(
                    self.provider_code.as_deref(),
                    Some("input_too_long" | "max_tokens_per_input_exceeded" | "tokens_exceeded")
                ),
            transient: matches!(self.status, Some(408 | 429 | 500..=599))
                || matches!(self.kind.as_str(), "transport" | "timeout"),
            rate_limited: self.status == Some(429),
        }
    }
    pub fn provider(status: Option<u16>, value: &Value) -> Self {
        let error = value.get("error").filter(|v| !v.is_null()).unwrap_or(value);
        Self {
            kind: "provider".into(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("Proxy returned an error without a message")
                .chars()
                .take(MAX_ERROR_MESSAGE_CHARS)
                .collect(),
            provider_code: error.get("code").and_then(Value::as_str).map(str::to_owned),
            status,
            stream_provider_status: None,
            http_observation: None,
            usage_limit: usage_limit::UsageLimit::parse_for_status(
                value,
                chrono::Utc::now(),
                status,
            )
            .map(Box::new),
        }
    }

    fn stream_provider(event: &Value, duplicate_relevant_key: bool) -> Self {
        let kind = event.get("type").and_then(Value::as_str);
        let payload = if matches!(kind, Some("response.failed" | "response.incomplete")) {
            event.get("response").unwrap_or(event)
        } else {
            event
        };
        let mut error = Self::provider(None, payload);
        error.stream_provider_status =
            stream_error::status_from_event(event, duplicate_relevant_key);
        error
    }
}

/// Independent facts: a transport error may also carry an HTTP status/code.
/// Keeping these orthogonal preserves each consumer's precedence rules.
pub(crate) struct EmbeddingFailure {
    pub(crate) credentials_or_credit: bool,
    pub(crate) invalid_configuration: bool,
    pub(crate) input_limit: bool,
    pub(crate) transient: bool,
    pub(crate) rate_limited: bool,
}

pub fn shell_tool() -> Value {
    json!({"type":"function","name":"shell","strict":false,
    "description":"Execute the command directly in Bash or PowerShell in this Thread cwd. For example, to start a background job call shell with command and background:true, then use a later shell call with command: shellboy shell get --execution ID or shellboy shell read --execution ID --encoding UTF-8. IDs are flag values, never positional arguments. To wait for completion use shellboy wait --conditions-json '[{\"type\":\"shell_ended\",\"execution_id\":\"ID\"}]' --timeout-ms 30000. execution_id identifies the process started by THIS call. If command is shellboy message send, that process sends a message and its stdout is the receipt JSON; its execution_id is NOT a shell requested in the message. Use shellboy --help for history, messages, children, wait and attachments. Foreground is default; background must be explicit.",
    "parameters":{"type":"object","additionalProperties":false,"required":["command","description"],"properties":{
        "command":{"type":"string"},"description":{"type":"string","maxLength":80,"description":"Briefly describe what this command actually does in the conversation language, at most 80 characters. Not a substitute for the command."},"background":{"type":"boolean"},"stdin_open":{"type":"boolean"},"initial_stdin":{"type":"string"},"input_encoding":{"type":"string"},"output_encoding":{"type":"string"}
    }}})
}
/// Map the role of Shellboy-inserted context on the outgoing copy only. The
/// leading `system` message stays as is; with `User`, every `developer` item
/// after it is sent as `user`. Its text, including the `Shellboy ... v1:` marker
/// that tells the model it is a notification, is not changed.
pub fn apply_context_role(request: &mut Value, role: crate::config::ContextRole) {
    if role == crate::config::ContextRole::Developer {
        return;
    }
    if let Some(items) = request["input"].as_array_mut() {
        for item in items {
            if item["role"] == "developer" {
                item["role"] = Value::String("user".into());
            }
        }
    }
}
pub fn configure_shell_description(request: &mut Value, enabled: bool) {
    if !enabled && let Some(tools) = request["tools"].as_array_mut() {
        for tool in tools {
            if tool["name"] == "shell"
                && let Some(properties) = tool["parameters"]["properties"].as_object_mut()
            {
                properties.remove("description");
                if let Some(required) = tool["parameters"]["required"].as_array_mut() {
                    required.retain(|field| field.as_str() != Some("description"));
                }
            }
        }
    }
}
/// Canonical request field carrying a Thread's `max_tokens` override. It reuses
/// the Responses key `max_output_tokens` because the canonical request is
/// Responses-shaped; the HTTP client removes it and sends the limit in each
/// dialect's own field (so it never reaches the wire under this name for Chat
/// or Messages, and Responses gets it from the encoder, not passthrough).
pub const THREAD_OUTPUT_LIMIT_FIELD: &str = "max_output_tokens";
/// Set the Thread's output-limit override on a canonical conversation request.
/// `None` keeps the connection value. Summary requests never call this.
pub fn apply_thread_max_tokens(request: &mut Value, max_tokens: Option<u64>) {
    if let Some(limit) = max_tokens {
        request[THREAD_OUTPUT_LIMIT_FIELD] = json!(limit);
    }
}
pub fn response_request(
    model: &str,
    input: Vec<Value>,
    effort: Option<&str>,
    tools_enabled: bool,
) -> Value {
    let mut request = json!({"model":model,"input":input,"store":false,"stream":true,"include":["reasoning.encrypted_content"],"tools":[]});
    if let Some(effort) = effort {
        request["reasoning"] = json!({"effort":effort});
    }
    if tools_enabled {
        request["tools"] = json!([shell_tool()]);
    }
    request
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub call_id: String,
    pub item_id: Option<String>,
    pub args: Result<ShellToolArgs, String>,
    pub unknown_arguments: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub raw: Value,
    pub output: Vec<Value>,
    pub calls: Vec<ToolCall>,
    pub text: String,
    pub stream_observation: Option<StreamObservation>,
    pub http_observation: Option<http::HttpObservation>,
    /// HTTP metadata, including absent observations for requests not sent.
    pub response_facts: http::ResponseFacts,
}
#[derive(Debug, Clone, Serialize)]
pub struct StreamObservation {
    pub differences: Vec<OutputMismatch>,
    /// Whole-response delta channels; no item index is inferred. At most two.
    pub delta_differences: Vec<OutputMismatchField>,
    pub total_differences: usize,
    pub partial_text: String,
    pub partial_reasoning: String,
    pub completed_items: BTreeMap<u64, Value>,
}
pub(crate) const SHELL_ARGUMENTS: [&str; 7] = [
    "command",
    "description",
    "background",
    "stdin_open",
    "initial_stdin",
    "input_encoding",
    "output_encoding",
];

struct UniqueShellObject(serde_json::Map<String, Value>);

impl<'de> serde::Deserialize<'de> for UniqueShellObject {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> serde::de::Visitor<'de> for ObjectVisitor {
            type Value = UniqueShellObject;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object with unique parameter names")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some(name) = map.next_key::<String>()? {
                    if object.contains_key(&name) {
                        return Err(serde::de::Error::custom(format!(
                            "Duplicate shell parameter {name}"
                        )));
                    }
                    object.insert(name, map.next_value()?);
                }
                Ok(UniqueShellObject(object))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn parse_shell_arguments(raw: &str) -> (Result<ShellToolArgs, String>, Vec<String>) {
    let mut unknown = Vec::new();
    let result = (|| {
        let UniqueShellObject(object) = serde_json::from_str(raw)
            .map_err(|error| format!("Invalid shell arguments: {error}"))?;
        let mut value = Value::Object(object);
        let object = value
            .as_object_mut()
            .ok_or_else(|| "Shell arguments must be a JSON object".to_owned())?;
        unknown = object
            .keys()
            .filter(|name| !SHELL_ARGUMENTS.contains(&name.as_str()))
            .cloned()
            .collect();
        for name in &unknown {
            object.remove(name);
        }
        for (name, value) in object.iter() {
            let valid = match name.as_str() {
                "command" | "input_encoding" | "output_encoding" => value.is_string(),
                "description" | "initial_stdin" => value.is_string() || value.is_null(),
                "background" | "stdin_open" => value.is_boolean(),
                _ => unreachable!(),
            };
            if !valid {
                let expected = if matches!(name.as_str(), "background" | "stdin_open") {
                    "boolean"
                } else {
                    "string"
                };
                return Err(format!(
                    "Invalid shell parameter {name}: expected {expected}"
                ));
            }
        }
        if !object.contains_key("command") {
            return Err("Missing shell parameter command: expected non-empty string".into());
        }
        for name in ["command", "input_encoding", "output_encoding"] {
            if let Some(text) = object.get(name).and_then(Value::as_str) {
                if text.trim().is_empty() {
                    return Err(format!(
                        "Invalid shell parameter {name}: expected non-empty string"
                    ));
                }
                if text.contains('\0') {
                    return Err(format!(
                        "Invalid shell parameter {name}: expected string without NUL"
                    ));
                }
            }
        }
        let args: ShellToolArgs = serde_json::from_value(value)
            .map_err(|_| "Invalid shell argument object".to_owned())?;
        args.validate().map_err(|error| error.message)?;
        Ok(args)
    })();
    (result, unknown)
}

fn output_limit_error(raw: &Value) -> Option<ModelError> {
    if raw.get("status").and_then(Value::as_str) != Some("incomplete") {
        return None;
    }
    let api = raw.get("api").and_then(Value::as_str);
    let limited = match api {
        Some("anthropic_messages") => {
            raw.get("stop_reason").and_then(Value::as_str) == Some("max_tokens")
        }
        Some("chat_completions") => {
            raw.get("finish_reason").and_then(Value::as_str) == Some("length")
        }
        Some("responses") | None => {
            raw.pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                == Some("max_output_tokens")
        }
        Some(_) => false,
    };
    limited.then(|| ModelError::new(
        "output_limit",
        "The response was cut off by the output limit. Raise max_tokens for this Thread (shellboy thread set --max-tokens N) or for this connection.",
    ))
}

impl ModelResponse {
    pub fn completed(mut raw: Value, tools_enabled: bool) -> Result<Self, ModelError> {
        crate::usage::normalize_response(&mut raw);
        cache_diagnostics::normalize_response(&mut raw);
        if raw.get("status").and_then(Value::as_str) != Some("completed") {
            return Err(output_limit_error(&raw)
                .unwrap_or_else(|| ModelError::new("incomplete", "Response was not completed")));
        }
        let output = raw
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| ModelError::new("protocol", "Completed response has no output array"))?
            .clone();
        let mut calls = Vec::new();
        let mut text = String::new();
        let mut ids = HashSet::new();
        for item in &output {
            if item
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| s != "completed")
            {
                return Err(ModelError::new(
                    "incomplete",
                    "Completed response contains an unfinished item",
                ));
            }
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if item.get("role").and_then(Value::as_str) != Some("assistant") {
                        return Err(ModelError::new(
                            "protocol",
                            "Output message is not assistant",
                        ));
                    }
                    let content =
                        item.get("content")
                            .and_then(Value::as_array)
                            .ok_or_else(|| {
                                ModelError::new("protocol", "Message has no content array")
                            })?;
                    for part in content {
                        let body = match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => part.get("text").and_then(Value::as_str),
                            Some("refusal") => part.get("refusal").and_then(Value::as_str),
                            _ => {
                                return Err(ModelError::new(
                                    "unsupported_item",
                                    "Unsupported message content; preserve original output",
                                ));
                            }
                        }
                        .ok_or_else(|| ModelError::new("protocol", "Output content has no text"))?;
                        text.push_str(body);
                    }
                }
                Some("reasoning") => {}
                // Kept and returned verbatim; only the envelope is checked.
                Some(opaque::TYPE) => {
                    opaque::validate(item)
                        .map_err(|message| ModelError::new("protocol", message))?;
                }
                Some("function_call") => {
                    if !tools_enabled {
                        return Err(ModelError::new("protocol", "Disabled or unknown function"));
                    }
                    let id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ModelError::new("protocol", "Missing call_id"))?;
                    if !ids.insert(id.to_owned()) {
                        return Err(ModelError::new("protocol", "Duplicate call_id"));
                    }
                    let arguments =
                        item.get("arguments")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ModelError::new("protocol", "Arguments are not a JSON string")
                            })?;
                    if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                        return Err(ModelError::new(
                            "input_limit",
                            "Tool arguments exceed byte limit",
                        ));
                    }
                    let (args, unknown_arguments) =
                        if item.get("name").and_then(Value::as_str) != Some("shell") {
                            (
                                Err("Unknown tool; only shell is available".into()),
                                Vec::new(),
                            )
                        } else {
                            parse_shell_arguments(arguments)
                        };
                    calls.push(ToolCall {
                        call_id: id.into(),
                        item_id: item.get("id").and_then(Value::as_str).map(str::to_owned),
                        args,
                        unknown_arguments,
                    });
                }
                _ => {
                    return Err(ModelError::new(
                        "unsupported_item",
                        "Unsupported output item; preserve original output",
                    ));
                }
            }
        }
        Ok(Self {
            raw,
            output,
            calls,
            text,
            stream_observation: None,
            http_observation: None,
            response_facts: http::ResponseFacts::default(),
        })
    }
}

#[derive(Clone)]
struct DecodedEvent {
    value: Value,
    duplicate_relevant_key: bool,
    untyped_error_candidate: bool,
}
struct DecodeFailure {
    error: ModelError,
    events: Vec<DecodedEvent>,
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    total: usize,
    parsed_total: usize,
    after_cr: bool,
    first_line_seen: bool,
    json_events: usize,
    done_markers: usize,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
        self.push_until(bytes, |_| false)
            .map(|events| events.into_iter().map(|event| event.value).collect())
            .map_err(|failure| failure.error)
    }
    /// Stop at a complete terminal frame before interpreting any trailing bytes.
    fn push_until(
        &mut self,
        bytes: &[u8],
        mut terminal: impl FnMut(&DecodedEvent) -> bool,
    ) -> Result<Vec<DecodedEvent>, DecodeFailure> {
        self.decode(bytes, &mut terminal, false)
    }
    fn decode(
        &mut self,
        bytes: &[u8],
        mut terminal: impl FnMut(&DecodedEvent) -> bool,
        stop_done: bool,
    ) -> Result<Vec<DecodedEvent>, DecodeFailure> {
        let mut events = Vec::new();
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or_else(|| DecodeFailure {
                error: ModelError::new("input_limit", "Response length overflow"),
                events: vec![],
            })?;
        for &byte in bytes {
            let Some(parsed_total) = self.parsed_total.checked_add(1) else {
                return Err(DecodeFailure {
                    error: ModelError::new("input_limit", "Response length overflow"),
                    events,
                });
            };
            self.parsed_total = parsed_total;
            if self.parsed_total > MAX_RESPONSE_BYTES {
                return Err(DecodeFailure {
                    error: ModelError::new("input_limit", "Response byte limit exceeded"),
                    events,
                });
            }
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte != b'\n' && byte != b'\r' {
                self.line.push(byte);
                if self.line.len() + self.data.len() > MAX_EVENT_BYTES {
                    return Err(DecodeFailure {
                        error: ModelError::new("input_limit", "SSE event byte limit exceeded"),
                        events,
                    });
                }
                continue;
            }
            // CR is a line end itself; consume only an immediately following LF.
            // Keep this across chunks, including a split CRLF pair.
            self.after_cr = byte == b'\r';
            let mut line = std::mem::take(&mut self.line);
            if !self.first_line_seen {
                self.first_line_seen = true;
                if line.starts_with(b"\xef\xbb\xbf") {
                    line.drain(..3);
                }
            }
            if line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop();
                    if self.data != b"[DONE]" {
                        let (event, duplicate_relevant_key, untyped_error_candidate) =
                            match stream_error::parse_frame(&self.data) {
                                Ok(parsed) => parsed,
                                Err(_) => {
                                    return Err(DecodeFailure {
                                        error: ModelError::new(
                                            "protocol",
                                            "Malformed UTF-8/JSON in SSE",
                                        ),
                                        events,
                                    });
                                }
                            };
                        self.json_events += 1;
                        let decoded = DecodedEvent {
                            value: event,
                            duplicate_relevant_key,
                            untyped_error_candidate,
                        };
                        let stop = terminal(&decoded);
                        events.push(decoded);
                        if stop {
                            self.data.clear();
                            break;
                        }
                    } else {
                        self.done_markers += 1;
                        if stop_done {
                            self.data.clear();
                            break;
                        }
                    }
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix(b"data:") {
                self.data
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                self.data.push(b'\n');
                if self.data.len() > MAX_EVENT_BYTES {
                    return Err(DecodeFailure {
                        error: ModelError::new("input_limit", "SSE event byte limit exceeded"),
                        events,
                    });
                }
            }
        }
        Ok(events)
    }
    pub fn is_at_boundary(&self) -> bool {
        self.line.is_empty() && self.data.is_empty()
    }
    fn eof_diagnostic(&self, completed_items: usize, last_event: Option<&str>) -> String {
        // Counts and a fixed classification only: never leak response text or event names.
        format!(
            "received_bytes={}, parsed_events={}, completed_items={}, done_markers={}, pending_line_bytes={}, pending_data_bytes={}, last_event={}",
            self.total,
            self.json_events,
            completed_items,
            self.done_markers,
            self.line.len(),
            self.data.len(),
            last_event.unwrap_or("none")
        )
    }
}

/// Shared semantic event classification for recovery facts and live display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamEventKind {
    Metadata,
    Output,
    TextDelta,
    ReasoningDelta,
}
pub(crate) fn classify_stream_event(event: &Value) -> StreamEventKind {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "response.output_text.delta" | "response.refusal.delta" => StreamEventKind::TextDelta,
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            StreamEventKind::ReasoningDelta
        }
        _ if kind.starts_with("response.output_")
            || kind.starts_with("response.content_part.")
            || kind.starts_with("response.function_call_arguments.")
            || kind.starts_with("response.reasoning")
            || kind.starts_with("response.refusal.")
            || event
                .get("response")
                .and_then(|response| response.get("output"))
                .and_then(Value::as_array)
                .is_some_and(|output| !output.is_empty()) =>
        {
            StreamEventKind::Output
        }
        _ => StreamEventKind::Metadata,
    }
}

#[derive(Debug, Default)]
pub struct ResponseStream {
    decoder: SseDecoder,
    pub partial_text: String,
    pub partial_reasoning: String,
    pub completed_items: BTreeMap<u64, Value>,
    terminal: Option<Result<Value, ModelError>>,
    failed_response: Option<Value>,
    differences: Vec<OutputMismatch>,
    total_differences: usize,
    last_event: Option<&'static str>,
    output_observed: bool,
}
impl ResponseStream {
    /// Parsed events can be persisted/previewed. Tools are never started here.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
        if self.terminal.is_some() {
            return Ok(Vec::new());
        }
        let decoded = self.decoder.push_until(bytes, |decoded| {
            let event = &decoded.value;
            matches!(
                event.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.failed" | "response.incomplete" | "error")
            ) || stream_error::recognized_untyped_error(event)
                || (decoded.untyped_error_candidate && event.get("type").is_none())
        });
        let (decoded_events, decode_error) = match decoded {
            Ok(events) => (events, None),
            Err(failure) => (failure.events, Some(failure.error)),
        };
        if decode_error.is_some() {
            self.failed_response = decoded_events
                .iter()
                .rev()
                .find_map(|event| event.value.get("response"))
                .cloned();
        }
        let mut events = Vec::with_capacity(decoded_events.len());
        // Apply the valid prefix before returning a later frame decode failure.
        for decoded in decoded_events {
            let event = decoded.value;
            let classification = classify_stream_event(&event);
            self.output_observed |= classification != StreamEventKind::Metadata;
            if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                match classification {
                    StreamEventKind::TextDelta => self.partial_text.push_str(delta),
                    StreamEventKind::ReasoningDelta => self.partial_reasoning.push_str(delta),
                    _ => {}
                }
            }
            self.last_event = Some(match classification {
                StreamEventKind::TextDelta => "text_delta",
                StreamEventKind::ReasoningDelta => "reasoning_delta",
                _ => match event.get("type").and_then(Value::as_str) {
                    Some("response.output_item.done") => "item_done",
                    Some(
                        "response.completed" | "response.failed" | "response.incomplete" | "error",
                    ) => "terminal",
                    Some(_) => "other",
                    None => "untyped",
                },
            });
            match event.get("type").and_then(Value::as_str) {
                Some("response.output_item.done") => {
                    let index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| ModelError::new("protocol", "Item done has no index"))?;
                    let item = event
                        .get("item")
                        .filter(|v| v.is_object())
                        .ok_or_else(|| ModelError::new("protocol", "Item done has no item"))?;
                    if self.completed_items.insert(index, item.clone()).is_some() {
                        return Err(ModelError::new("protocol", "Duplicate item done"));
                    }
                }
                Some("response.completed") => {
                    let response = event.get("response").ok_or_else(|| {
                        ModelError::new("protocol", "Completed event has no response")
                    })?;
                    let Some(output) = response.get("output").and_then(Value::as_array) else {
                        let error = ModelError::new("protocol", "Completed response has no output");
                        self.failed_response = Some(response.clone());
                        self.terminal = Some(Err(error.clone()));
                        return Err(error);
                    };
                    for (index, item) in &self.completed_items {
                        let final_item = usize::try_from(*index)
                            .ok()
                            .and_then(|index| output.get(index));
                        if final_item != Some(item) {
                            self.total_differences += 1;
                            if self.differences.len() < 16 {
                                let field = final_item.map_or(
                                    OutputMismatchField::OutputIndex,
                                    |final_item| {
                                        reconcile::mismatch(item, final_item)
                                            .unwrap_or(OutputMismatchField::Metadata)
                                    },
                                );
                                self.differences.push(OutputMismatch {
                                    output_index: *index,
                                    field,
                                });
                            }
                        }
                    }
                    self.terminal = Some(Ok(response.clone()));
                }
                Some("response.failed" | "response.incomplete") => {
                    let response = event.get("response").unwrap_or(&event);
                    self.failed_response = Some(response.clone());
                    let error = if response.get("error").is_some_and(|v| !v.is_null()) {
                        ModelError::stream_provider(&event, decoded.duplicate_relevant_key)
                    } else if event.get("type").and_then(Value::as_str)
                        == Some("response.incomplete")
                    {
                        output_limit_error(response).unwrap_or_else(|| {
                            ModelError::new("incomplete", "Proxy did not complete response")
                        })
                    } else {
                        ModelError::new("incomplete", "Proxy did not complete response")
                    };
                    self.terminal = Some(Err(error));
                }
                Some("error") => {
                    self.terminal = Some(Err(ModelError::stream_provider(
                        &event,
                        decoded.duplicate_relevant_key,
                    )));
                }
                None if stream_error::recognized_untyped_error(&event)
                    || (decoded.untyped_error_candidate && event.get("type").is_none()) =>
                {
                    self.failed_response = Some(event.clone());
                    self.terminal = Some(Err(ModelError::stream_provider(
                        &event,
                        decoded.duplicate_relevant_key,
                    )));
                }
                _ => {}
            }
            events.push(event);
        }
        if let Some(error) = decode_error {
            return Err(error);
        }
        Ok(events)
    }
    /// Whether any generated output item, content, or delta was observed.
    /// Transport bytes, comments, created, and in-progress metadata alone do not count.
    pub fn has_output(&self) -> bool {
        self.output_observed
    }
    pub fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }
    pub(crate) fn attach_stream_provider_status(&mut self, status: Option<StreamProviderStatus>) {
        if let Some(Err(error)) = &mut self.terminal {
            error.stream_provider_status = status;
        }
    }
    pub(crate) fn failed_response(&self) -> Option<&Value> {
        self.failed_response.as_ref()
    }
    pub fn finish(self, tools_enabled: bool) -> Result<ModelResponse, ModelError> {
        if !self.decoder.is_at_boundary() {
            return Err(ModelError::new(
                "incomplete",
                format!(
                    "EOF in partial SSE event ({})",
                    self.decoder
                        .eof_diagnostic(self.completed_items.len(), self.last_event)
                ),
            ));
        }
        match self.terminal {
            Some(Ok(raw)) => {
                let mut response = ModelResponse::completed(raw, tools_enabled)?;
                let delta_differences = reconcile::delta_differences(
                    &self.partial_text,
                    &self.partial_reasoning,
                    &response,
                );
                let total_differences = self.total_differences + delta_differences.len();
                if total_differences > 0 {
                    response.stream_observation = Some(StreamObservation {
                        differences: self.differences,
                        delta_differences,
                        total_differences,
                        partial_text: self.partial_text,
                        partial_reasoning: self.partial_reasoning,
                        completed_items: self.completed_items,
                    });
                }
                Ok(response)
            }
            Some(Err(e)) => Err(e),
            None => Err(ModelError::new(
                "incomplete",
                format!(
                    "EOF before response.completed ({})",
                    self.decoder
                        .eof_diagnostic(self.completed_items.len(), self.last_event)
                ),
            )),
        }
    }
}

#[cfg(test)]
mod shell_argument_policy_tests {
    use super::*;
    #[test]
    fn unknowns_are_separate_from_valid_and_invalid_fields() {
        let (args, unknown) = parse_shell_arguments(r#"{"command":"true","timeout":2}"#);
        assert!(args.is_ok());
        assert_eq!(unknown, ["timeout"]);
        let (args, unknown) = parse_shell_arguments(r#"{"command":1,"timeout":2}"#);
        assert!(args.unwrap_err().contains("command"));
        assert_eq!(unknown, ["timeout"]);
        for raw in [r#"{}"#, r#"{"command":""}"#, r#"{"command":false}"#] {
            assert!(
                parse_shell_arguments(raw)
                    .0
                    .unwrap_err()
                    .contains("command")
            );
        }
    }
    #[test]
    fn duplicate_known_and_unknown_keys_are_rejected() {
        for (raw, name) in [
            (r#"{"command":"first","command":"second"}"#, "command"),
            (r#"{"command":"true","timeout":1,"timeout":2}"#, "timeout"),
            (r#"{"command":"true","\u0063ommand":"false"}"#, "command"),
        ] {
            let error = parse_shell_arguments(raw).0.unwrap_err();
            assert!(error.contains("Duplicate shell parameter"), "{error}");
            assert!(error.contains(name), "{error}");
        }
    }
    #[test]
    fn byte_limit_and_duplicate_ids_remain_protocol_errors() {
        let item = |id: &str, arguments: String| json!({"type":"function_call","name":"shell","call_id":id,"arguments":arguments});
        let large = "x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1);
        let raw = json!({"status":"completed","output":[item("a",large)]});
        assert!(ModelResponse::completed(raw, true).is_err());
        let raw =
            json!({"status":"completed","output":[item("a","{}".into()),item("a","{}".into())]});
        assert!(ModelResponse::completed(raw, true).is_err());
    }
}

#[cfg(test)]
mod embedding_failure_tests {
    use super::*;
    #[test]
    fn embedding_facts_preserve_overlapping_causes_without_consumer_policy() {
        for status in [401, 402, 403] {
            let facts =
                ModelError::provider(Some(status), &json!({"error":{"code":"input_too_long"}}))
                    .embedding_failure();
            assert!(facts.credentials_or_credit && facts.input_limit);
            assert!(!facts.rate_limited || facts.transient);
        }
        let rate = ModelError::provider(Some(429), &json!({})).embedding_failure();
        assert!(rate.rate_limited && rate.transient);
        assert!(!rate.credentials_or_credit);
        assert!(
            ModelError::new("timeout", "bounded")
                .embedding_failure()
                .transient
        );
        assert!(
            !ModelError::new("cancelled", "bounded")
                .embedding_failure()
                .transient
        );
        let mut transport = ModelError::new("transport", "bounded");
        transport.status = Some(402);
        let facts = transport.embedding_failure();
        assert!(facts.transient && facts.credentials_or_credit);
        for kind in [
            "config_missing",
            "config_missing_credentials",
            "dimension_mismatch",
        ] {
            assert!(
                ModelError::new(kind, "bounded")
                    .embedding_failure()
                    .invalid_configuration
            );
        }
    }
}

#[cfg(test)]
mod s1b_stream_error_tests {
    use super::*;

    #[test]
    fn t10_absent_type_top_level_error_is_a_first_terminal_frame() {
        let envelope =
            b"data: {\"error\":{\"code\":\"400\",\"message\":\"SECRET_PROVIDER_MESSAGE\"}}\n\n";
        let mut stream = ResponseStream::default();
        stream.push(envelope).unwrap();
        assert!(
            stream.is_terminal(),
            "recognized untyped top-level error must stop at its complete frame"
        );
        let error = stream.finish(false).unwrap_err();
        assert_eq!(error.kind, "provider");
        assert_eq!(
            error.status, None,
            "embedded status is not actual HTTP status"
        );
    }

    #[test]
    fn stream_status_accepts_only_exact_ascii_code_strings() {
        for (code, expected) in [
            ("100", Some(100)),
            ("200", Some(200)),
            ("400", Some(400)),
            ("429", Some(429)),
            ("599", Some(599)),
            ("099", None),
            ("600", None),
            ("0400", None),
            ("400 ", None),
            (" 400", None),
            ("+400", None),
            ("4e2", None),
            ("400.0", None),
            ("٤٠٠", None),
            ("context_length_exceeded", None),
        ] {
            let body = format!("data: {{\"error\":{{\"code\":\"{code}\"}}}}\n\n");
            let mut stream = ResponseStream::default();
            stream.push(body.as_bytes()).unwrap();
            let error = stream.finish(false).unwrap_err();
            assert_eq!(error.kind, "provider", "{code}");
            assert_eq!(
                error.status, None,
                "embedded code is never actual HTTP status"
            );
            assert_eq!(
                error.stream_provider_status.map(|status| status.value),
                expected,
                "{code}"
            );
            assert_eq!(
                error.stream_provider_status.map(|status| status.source),
                expected.map(|_| StreamProviderStatusSource::StreamErrorCode),
                "{code}"
            );
        }
        for code in [json!(400), json!(400.0), Value::Null, json!(true)] {
            let body = json!({"error":{"code":code}}).to_string();
            let frame = format!("data: {body}\n\n");
            let mut stream = ResponseStream::default();
            stream.push(frame.as_bytes()).unwrap();
            assert!(
                stream
                    .finish(false)
                    .unwrap_err()
                    .stream_provider_status
                    .is_none()
            );
        }
    }

    #[test]
    fn duplicate_error_code_withholds_status_but_keeps_terminal_error() {
        let bytes = b"data: {\"error\":{\"code\":\"401\",\"code\":\"400\"}}\n\n";
        let mut stream = ResponseStream::default();
        stream.push(bytes).unwrap();
        assert!(stream.is_terminal());
        let error = stream.finish(false).unwrap_err();
        assert_eq!(error.kind, "provider");
        assert!(error.stream_provider_status.is_none());
    }

    #[test]
    fn typed_response_failed_error_uses_only_its_nested_error_code() {
        let mut stream = ResponseStream::default();
        stream.push(b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"401\",\"status\":599}}}\n\n").unwrap();
        let error = stream.finish(false).unwrap_err();
        assert_eq!(error.kind, "provider");
        assert_eq!(error.status, None);
        assert_eq!(
            error.stream_provider_status.map(|status| status.value),
            Some(401)
        );
    }

    #[test]
    fn duplicate_root_error_keeps_terminality_but_withholds_provider_status() {
        let bytes = b"data: {\"error\":{\"code\":\"400\"},\"error\":null}\n\n";
        let mut stream = ResponseStream::default();
        stream.push(bytes).unwrap();
        assert!(stream.is_terminal());
        let error = stream.finish(false).unwrap_err();
        assert_eq!(error.kind, "provider");
        assert!(error.stream_provider_status.is_none());
    }

    #[test]
    fn typed_error_stays_terminal_but_unknown_present_type_and_nested_error_do_not() {
        let mut typed = ResponseStream::default();
        typed
            .push(b"data: {\"type\":\"error\",\"error\":{\"code\":\"429\"}}\n\n")
            .unwrap();
        assert_eq!(
            typed
                .finish(false)
                .unwrap_err()
                .stream_provider_status
                .map(|s| s.value),
            Some(429)
        );
        for body in [
            b"data: {\"type\":\"future\",\"error\":{\"code\":\"400\"}}\n\n".as_slice(),
            b"data: {\"type\":null,\"error\":{\"code\":\"400\"}}\n\n".as_slice(),
            b"data: {\"response\":{\"error\":{\"code\":\"400\"}}}\n\n".as_slice(),
            b"data: {\"error\":null}\n\n".as_slice(),
        ] {
            let mut stream = ResponseStream::default();
            stream.push(body).unwrap();
            assert!(
                !stream.is_terminal(),
                "unexpected terminal for {:?}",
                String::from_utf8_lossy(body)
            );
            assert_eq!(stream.finish(false).unwrap_err().kind, "incomplete");
        }
    }

    #[test]
    fn completion_before_error_remains_success_and_preterminal_bad_frame_remains_protocol() {
        let mut stream = ResponseStream::default();
        stream.push(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\ndata: {\"error\":{\"code\":\"400\"}}\n\n").unwrap();
        assert!(stream.finish(false).is_ok());
        let mut stream = ResponseStream::default();
        assert_eq!(
            stream
                .push(b"data: not-json\n\ndata: {\"error\":{\"code\":\"400\"}}\n\n")
                .unwrap_err()
                .kind,
            "protocol"
        );
    }

    #[test]
    fn response_byte_cap_is_charged_only_through_the_terminal_prefix() {
        let terminal = b"data: {\"error\":{\"code\":\"400\"}}\n\n";
        let mut stream = ResponseStream::default();
        stream.decoder.parsed_total = MAX_RESPONSE_BYTES - terminal.len();
        let mut bytes = terminal.to_vec();
        bytes.extend_from_slice(b"data: not-json\n\n");
        stream.push(&bytes).unwrap();
        assert!(stream.is_terminal());
        assert_eq!(stream.decoder.parsed_total, MAX_RESPONSE_BYTES);
        assert_eq!(stream.decoder.total, bytes.len());
        let mut preterminal = SseDecoder {
            parsed_total: MAX_RESPONSE_BYTES,
            ..Default::default()
        };
        assert_eq!(preterminal.push(b"x").unwrap_err().kind, "input_limit");
    }

    #[test]
    fn t10_first_terminal_ignores_malformed_same_chunk_suffix() {
        let bytes = b"data: {\"error\":{\"code\":\"400\"}}\n\ndata: not-json\n\n";
        let mut stream = ResponseStream::default();
        stream.push(bytes).unwrap();
        assert!(
            stream.is_terminal(),
            "first complete terminal frame owns the chunk"
        );
        assert_eq!(stream.finish(false).unwrap_err().kind, "provider");
    }

    #[test]
    fn t11_real_eof_without_terminal_remains_incomplete_control() {
        let mut stream = ResponseStream::default();
        stream
            .push(b"data: {\"id\":\"m\",\"choices\":[]}\n\n")
            .unwrap();
        let error = stream.finish(false).unwrap_err();
        assert_eq!(error.kind, "incomplete");
    }
}
