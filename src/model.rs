//! Bounded Responses SSE parsing and completed-response validation.

pub mod dialect;
pub mod http;
mod reconcile;
pub mod usage_limit;

use crate::protocol::ShellToolArgs;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

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
                .unwrap_or("Proxy returned an error without a message")
                .chars()
                .take(MAX_ERROR_MESSAGE_CHARS)
                .collect(),
            provider_code: error.get("code").and_then(Value::as_str).map(str::to_owned),
            status,
            http_observation: None,
            usage_limit: usage_limit::UsageLimit::parse(value, chrono::Utc::now()).map(Box::new),
        }
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
    "parameters":{"type":"object","additionalProperties":false,"required":["command"],"properties":{
        "command":{"type":"string"},"background":{"type":"boolean"},"stdin_open":{"type":"boolean"},"initial_stdin":{"type":"string"},"input_encoding":{"type":"string"},"output_encoding":{"type":"string"}
    }}})
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
}
#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub raw: Value,
    pub output: Vec<Value>,
    pub calls: Vec<ToolCall>,
    pub text: String,
    pub stream_observation: Option<StreamObservation>,
    pub http_observation: Option<http::HttpObservation>,
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
impl ModelResponse {
    pub fn completed(mut raw: Value, tools_enabled: bool) -> Result<Self, ModelError> {
        crate::usage::normalize_response(&mut raw);
        if raw.get("status").and_then(Value::as_str) != Some("completed") {
            return Err(ModelError::new("incomplete", "Response was not completed"));
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
                    let args = if item.get("name").and_then(Value::as_str) != Some("shell") {
                        Err("Unknown tool; only shell is available".into())
                    } else {
                        serde_json::from_str::<ShellToolArgs>(arguments)
                            .map_err(|_| {
                                "Invalid shell arguments; no calls in this group may execute"
                                    .to_owned()
                            })
                            .and_then(|args| {
                                args.validate().map_err(|error| error.message)?;
                                Ok(args)
                            })
                    };
                    calls.push(ToolCall {
                        call_id: id.into(),
                        item_id: item.get("id").and_then(Value::as_str).map(str::to_owned),
                        args,
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
        })
    }
}

struct DecodeFailure {
    error: ModelError,
    events: Vec<Value>,
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    total: usize,
    after_cr: bool,
    first_line_seen: bool,
    json_events: usize,
    done_markers: usize,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
        self.push_until(bytes, |_| false)
            .map_err(|failure| failure.error)
    }
    /// Stop at a complete terminal frame before interpreting any trailing bytes.
    fn push_until(
        &mut self,
        bytes: &[u8],
        mut terminal: impl FnMut(&Value) -> bool,
    ) -> Result<Vec<Value>, DecodeFailure> {
        self.decode(bytes, &mut terminal, false)
    }
    fn decode(
        &mut self,
        bytes: &[u8],
        mut terminal: impl FnMut(&Value) -> bool,
        stop_done: bool,
    ) -> Result<Vec<Value>, DecodeFailure> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or_else(|| DecodeFailure {
                error: ModelError::new("input_limit", "Response length overflow"),
                events: vec![],
            })?;
        if self.total > MAX_RESPONSE_BYTES {
            return Err(DecodeFailure {
                error: ModelError::new("input_limit", "Response byte limit exceeded"),
                events: vec![],
            });
        }
        let mut events = Vec::new();
        for &byte in bytes {
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
                        let event = match serde_json::from_slice(&self.data) {
                            Ok(event) => event,
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
                        let stop = terminal(&event);
                        events.push(event);
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
        let decoded = self.decoder.push_until(bytes, |event| {
            matches!(
                event.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.failed" | "response.incomplete" | "error")
            )
        });
        let (events, decode_error) = match decoded {
            Ok(events) => (events, None),
            Err(failure) => (failure.events, Some(failure.error)),
        };
        if decode_error.is_some() {
            self.failed_response = events
                .iter()
                .rev()
                .find_map(|event| event.get("response"))
                .cloned();
        }
        // Apply the valid prefix before returning a later frame decode failure.
        for event in &events {
            let classification = classify_stream_event(event);
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
                    let response = event.get("response").unwrap_or(event);
                    let error = if response.get("error").is_some_and(|v| !v.is_null()) {
                        ModelError::provider(None, response)
                    } else {
                        ModelError::new("incomplete", "Proxy did not complete response")
                    };
                    self.terminal = Some(Err(error));
                }
                Some("error") => {
                    self.terminal = Some(Err(ModelError::provider(None, event)));
                }
                _ => {}
            }
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
