//! Bounded Responses SSE parsing and completed-response validation.

pub mod http;

use crate::protocol::ShellToolArgs;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

pub const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct ModelError {
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}
impl ModelError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            provider_code: None,
            status: None,
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
                || self.kind == "transport",
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
                .take(16_384)
                .collect(),
            provider_code: error.get("code").and_then(Value::as_str).map(str::to_owned),
            status,
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
}
impl ModelResponse {
    pub fn completed(raw: Value, tools_enabled: bool) -> Result<Self, ModelError> {
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
        })
    }
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    total: usize,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
        self.push_until(bytes, |_| false)
    }
    /// Stop at a complete terminal frame before interpreting any trailing bytes.
    fn push_until(
        &mut self,
        bytes: &[u8],
        mut terminal: impl FnMut(&Value) -> bool,
    ) -> Result<Vec<Value>, ModelError> {
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or_else(|| ModelError::new("input_limit", "Response length overflow"))?;
        if self.total > MAX_RESPONSE_BYTES {
            return Err(ModelError::new(
                "input_limit",
                "Response byte limit exceeded",
            ));
        }
        let mut events = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                if self.line.len() + self.data.len() > MAX_EVENT_BYTES {
                    return Err(ModelError::new(
                        "input_limit",
                        "SSE event byte limit exceeded",
                    ));
                }
                continue;
            }
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop();
                    if self.data != b"[DONE]" {
                        let event = serde_json::from_slice(&self.data).map_err(|_| {
                            ModelError::new("protocol", "Malformed UTF-8/JSON in SSE")
                        })?;
                        let stop = terminal(&event);
                        events.push(event);
                        if stop {
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
                    return Err(ModelError::new(
                        "input_limit",
                        "SSE event byte limit exceeded",
                    ));
                }
            }
        }
        Ok(events)
    }
    pub fn is_at_boundary(&self) -> bool {
        self.line.is_empty() && self.data.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct ResponseStream {
    decoder: SseDecoder,
    pub partial_text: String,
    pub completed_items: BTreeMap<u64, Value>,
    terminal: Option<Result<Value, ModelError>>,
}
impl ResponseStream {
    /// Parsed events can be persisted/previewed. Tools are never started here.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
        if self.terminal.is_some() {
            return Ok(Vec::new());
        }
        let events = self.decoder.push_until(bytes, |event| {
            matches!(
                event.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.failed" | "response.incomplete" | "error")
            )
        })?;
        for event in &events {
            match event.get("type").and_then(Value::as_str) {
                Some("response.output_text.delta" | "response.refusal.delta") => {
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        self.partial_text.push_str(delta);
                    }
                }
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
                    let output = response
                        .get("output")
                        .and_then(Value::as_array)
                        .ok_or_else(|| {
                            ModelError::new("protocol", "Completed response has no output")
                        })?;
                    for (index, item) in &self.completed_items {
                        if output.get(*index as usize) != Some(item) {
                            return Err(ModelError::new(
                                "protocol",
                                "Completed output disagrees with item done",
                            ));
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
        Ok(events)
    }
    pub fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }
    pub fn finish(self, tools_enabled: bool) -> Result<ModelResponse, ModelError> {
        if !self.decoder.is_at_boundary() {
            return Err(ModelError::new("incomplete", "EOF in partial SSE event"));
        }
        match self.terminal {
            Some(Ok(raw)) => ModelResponse::completed(raw, tools_enabled),
            Some(Err(e)) => Err(e),
            None => Err(ModelError::new(
                "incomplete",
                "EOF before response.completed",
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
