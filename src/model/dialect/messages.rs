//! Anthropic Messages wire adapter (#120 stage 3). Sources: the public Anthropic
//! documentation read on 2026-11-01; no real provider call has been made.
//! The only optional field Shellboy sends is the Thread effort, as
//! `output_config.effort` (#126). Thinking, sampling, tool_choice and
//! cache_control are the user's, through `extra_body` (#119).
use super::*;
use crate::model::opaque;

pub(super) const DIALECT: &str = "anthropic_messages";
/// Sent as the `anthropic-version` header. Raising it is a deliberate change.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

fn text_of(content: &Value) -> Result<String, ModelError> {
    if let Some(text) = content.as_str() {
        return Ok(text.to_owned());
    }
    let parts = content
        .as_array()
        .ok_or_else(|| protocol("Message content must be text or an array"))?;
    let mut text = String::new();
    for part in parts {
        match part["type"].as_str() {
            Some("input_text" | "output_text") => {
                text.push_str(part["text"].as_str().unwrap_or_default());
            }
            Some("refusal") => text.push_str(part["refusal"].as_str().unwrap_or_default()),
            _ => {
                return Err(ModelError::new(
                    "unsupported_item",
                    "Unsupported system content for Anthropic Messages; no request sent",
                ));
            }
        }
    }
    Ok(text)
}

fn blocks_of(content: &Value) -> Result<Vec<Value>, ModelError> {
    if let Some(text) = content.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    let parts = content
        .as_array()
        .ok_or_else(|| protocol("Message content must be text or an array"))?;
    let mut blocks = Vec::new();
    for part in parts {
        match part["type"].as_str() {
            Some("input_text" | "output_text") => {
                blocks.push(json!({"type":"text","text":part["text"]}));
            }
            Some("refusal") => blocks.push(json!({"type":"text","text":part["refusal"]})),
            Some("input_image") => {
                let url = part["image_url"]
                    .as_str()
                    .ok_or_else(|| protocol("Image has no URL"))?;
                blocks.push(image_block(url)?);
            }
            _ => {
                return Err(ModelError::new(
                    "unsupported_item",
                    "Unsupported Messages input content; no request sent",
                ));
            }
        }
    }
    Ok(blocks)
}

fn image_block(url: &str) -> Result<Value, ModelError> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest
            .split_once(',')
            .ok_or_else(|| protocol("Malformed image data URL"))?;
        let media = meta
            .strip_suffix(";base64")
            .ok_or_else(|| protocol("Only base64 image data URLs are supported"))?;
        return Ok(
            json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}}),
        );
    }
    Ok(json!({"type":"image","source":{"type":"url","url":url}}))
}

/// Append blocks to the last message when it has the same role, else start one.
/// Messages requires alternating roles, so adjacent items of one role merge.
fn push(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut()
        && last["role"] == role
    {
        last["content"]
            .as_array_mut()
            .expect("content array")
            .extend(blocks);
        return;
    }
    messages.push(json!({"role":role,"content":blocks}));
}

/// The single effort mapping for this dialect (#126). Shellboy's effort is an
/// opaque endpoint string; only names known to differ are translated, and every
/// other value (`low`, `medium`, `high`, `xhigh`, `max`, ...) goes unchanged.
fn effort_name(effort: &str) -> &str {
    match effort {
        // Responses' lowest tier; Messages has no `minimal`.
        "minimal" => "low",
        other => other,
    }
}

pub(super) fn encode(request: Value, max_tokens: u64) -> Result<Value, ModelError> {
    let input = request["input"]
        .as_array()
        .ok_or_else(|| protocol("Conversation input must be an array"))?;
    let mut system: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for item in input {
        if opaque::is_opaque(item) {
            // Only this dialect's own blocks go back, unchanged, in place.
            if let Some(block) = opaque::block_for(item, DIALECT) {
                push(&mut messages, "assistant", vec![block.clone()]);
            }
            continue;
        }
        match item["type"].as_str() {
            Some("reasoning") => {}
            Some("function_call") => {
                let arguments = item["arguments"].as_str().unwrap_or("{}");
                let input: Value = serde_json::from_str(arguments)
                    .map_err(|_| protocol("Tool call arguments are not valid JSON"))?;
                let block = json!({"type":"tool_use","id":item["call_id"],"name":item["name"],"input":input});
                push(&mut messages, "assistant", vec![block]);
            }
            Some("function_call_output") => {
                let output = item["output"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| item["output"].to_string());
                let block =
                    json!({"type":"tool_result","tool_use_id":item["call_id"],"content":output});
                // Results of one assistant turn stay together and come first.
                let results_first = messages.last().is_some_and(|last| last["role"] == "user");
                if results_first {
                    let last = messages.last_mut().expect("user message");
                    let content = last["content"].as_array_mut().expect("content array");
                    let at = content
                        .iter()
                        .take_while(|b| b["type"] == "tool_result")
                        .count();
                    content.insert(at, block);
                } else {
                    push(&mut messages, "user", vec![block]);
                }
            }
            Some("message") | None => {
                let role = item["role"].as_str();
                match role {
                    Some("system" | "developer") if messages.is_empty() => {
                        system.push(text_of(&item["content"])?);
                    }
                    // Inserted context after the conversation began is a user
                    // message: Messages has no mid-conversation system role.
                    Some("system" | "developer" | "user") => {
                        push(&mut messages, "user", blocks_of(&item["content"])?);
                    }
                    Some("assistant") => {
                        push(&mut messages, "assistant", blocks_of(&item["content"])?);
                    }
                    _ => return Err(protocol("Unsupported conversation message role")),
                }
            }
            _ => {
                return Err(ModelError::new(
                    "unsupported_item",
                    "Unsupported Messages replay item; no request sent",
                ));
            }
        }
    }
    if messages.last().is_none_or(|last| last["role"] != "user") {
        return Err(ModelError::new(
            "input_invalid",
            "A Messages request must end with a user message; refusing to send an assistant prefill",
        ));
    }
    let mut result = json!({
        "model":request["model"],
        "max_tokens":max_tokens,
        "stream":true,
        "messages":messages,
    });
    if let Some(effort) = request.pointer("/reasoning/effort").and_then(Value::as_str) {
        result["output_config"] = json!({"effort":effort_name(effort)});
    }
    if !system.is_empty() {
        result["system"] = json!(system.join("\n\n"));
    }
    if let Some(tools) = request["tools"]
        .as_array()
        .filter(|tools| !tools.is_empty())
    {
        let converted = tools
            .iter()
            .map(|tool| {
                if tool["type"] != "function" {
                    return Err(protocol("Unsupported tool definition"));
                }
                let mut out = json!({"name":tool["name"],"input_schema":tool["parameters"]});
                if let Some(description) = tool.get("description").filter(|d| !d.is_null()) {
                    out["description"] = description.clone();
                }
                Ok(out)
            })
            .collect::<Result<Vec<_>, ModelError>>()?;
        result["tools"] = json!(converted);
    }
    Ok(result)
}

/// One content block being accumulated, by the provider's block index.
enum Block {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
    /// thinking, redacted_thinking and unknown types: kept unread, in order.
    Opaque {
        kind: String,
        block: Value,
    },
}

#[derive(Default)]
pub(super) struct Messages {
    pub(super) decoder: SseDecoder,
    id: Option<String>,
    model: Option<String>,
    blocks: BTreeMap<u64, Block>,
    stop_reason: Option<String>,
    input: Option<u64>,
    cache_creation: Option<u64>,
    cache_read: Option<u64>,
    output: Option<u64>,
    pub(super) stopped: bool,
    unknown_events: u64,
}

fn count(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n <= i64::MAX as u64)
}

impl Messages {
    fn take_usage(&mut self, usage: &Value) {
        for (key, slot) in [
            ("input_tokens", &mut self.input),
            ("cache_creation_input_tokens", &mut self.cache_creation),
            ("cache_read_input_tokens", &mut self.cache_read),
            ("output_tokens", &mut self.output),
        ] {
            // message_delta usage is cumulative; a field that is absent keeps
            // the earlier value, a present one replaces it.
            if usage.get(key).is_some() {
                *slot = count(usage, key);
            }
        }
    }

    /// The Responses-shaped usage for the counters reported so far.
    pub(super) fn usage(&self) -> Option<Value> {
        let reported = [
            self.input,
            self.cache_creation,
            self.cache_read,
            self.output,
        ]
        .iter()
        .any(Option::is_some);
        let total_input = self
            .input
            .zip(self.cache_creation)
            .zip(self.cache_read)
            .and_then(|((input, creation), read)| input.checked_add(creation)?.checked_add(read))
            .filter(|total| *total <= i64::MAX as u64);
        if !reported {
            return None;
        }
        let mut usage = serde_json::Map::new();
        if let Some(total) = total_input {
            usage.insert("input_tokens".into(), total.into());
        }
        if let Some(read) = self.cache_read {
            usage.insert("input_tokens_details".into(), json!({"cached_tokens":read}));
        }
        if let Some(output) = self.output {
            usage.insert("output_tokens".into(), output.into());
        }
        Some(crate::usage::StandardUsage::from_value(&Value::Object(usage)).to_value())
    }

    /// Convert one provider event into Responses-shaped events.
    pub(super) fn item(&mut self, event: &Value) -> Result<Vec<Value>, ModelError> {
        let mut out = Vec::new();
        match event["type"].as_str() {
            Some("ping") => {}
            Some("error") => {
                return Ok(vec![json!({"type":"error","error":{
                    "message":event["error"]["message"],
                    "code":event["error"]["type"],
                }})]);
            }
            Some("message_start") => {
                let message = &event["message"];
                if let Some(id) = message["id"].as_str() {
                    if id.len() > 1024 {
                        return Err(ModelError::new(
                            "input_limit",
                            "Messages response id exceeds byte limit",
                        ));
                    }
                    self.id = Some(id.into());
                }
                self.model = message["model"].as_str().map(str::to_owned);
                if let Some(usage) = message.get("usage") {
                    self.take_usage(usage);
                }
            }
            Some("content_block_start") => {
                let index = event["index"]
                    .as_u64()
                    .filter(|i| *i < 1024)
                    .ok_or_else(|| protocol("Invalid Messages block index"))?;
                if self.blocks.contains_key(&index) {
                    return Err(protocol("Duplicate Messages block index"));
                }
                let block = &event["content_block"];
                let kind = block["type"].as_str().unwrap_or("unknown");
                let entry = match kind {
                    "text" => Block::Text(block["text"].as_str().unwrap_or_default().to_owned()),
                    "tool_use" => Block::Tool {
                        id: block["id"].as_str().unwrap_or_default().to_owned(),
                        name: block["name"].as_str().unwrap_or_default().to_owned(),
                        json: String::new(),
                    },
                    other => Block::Opaque {
                        kind: other.chars().take(64).collect(),
                        block: block.clone(),
                    },
                };
                if let Block::Text(text) = &entry
                    && !text.is_empty()
                {
                    out.push(json!({"type":"response.output_text.delta","delta":text}));
                }
                self.blocks.insert(index, entry);
            }
            Some("content_block_delta") => {
                let index = event["index"]
                    .as_u64()
                    .ok_or_else(|| protocol("Invalid Messages block index"))?;
                let delta = &event["delta"];
                let block = self
                    .blocks
                    .get_mut(&index)
                    .ok_or_else(|| protocol("Messages delta for an unopened block"))?;
                match (block, delta["type"].as_str()) {
                    (Block::Text(text), Some("text_delta")) => {
                        let piece = delta["text"].as_str().unwrap_or_default();
                        text.push_str(piece);
                        if !piece.is_empty() {
                            out.push(json!({"type":"response.output_text.delta","delta":piece}));
                        }
                    }
                    (Block::Tool { json, .. }, Some("input_json_delta")) => {
                        let piece = delta["partial_json"].as_str().unwrap_or_default();
                        if json.len().saturating_add(piece.len()) > MAX_TOOL_ARGUMENT_BYTES {
                            return Err(ModelError::new(
                                "input_limit",
                                "Messages tool arguments exceed byte limit",
                            ));
                        }
                        json.push_str(piece);
                        out.push(
                            json!({"type":"response.function_call_arguments.delta","delta":piece}),
                        );
                    }
                    (Block::Opaque { block, .. }, Some(kind)) => {
                        // thinking_delta, signature_delta and any future delta:
                        // append string fields to the block without reading them.
                        accumulate(block, delta, kind);
                    }
                    _ => return Err(protocol("Messages delta does not match its block")),
                }
            }
            Some("content_block_stop") => {}
            Some("message_delta") => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    if reason.len() > 64 {
                        return Err(ModelError::new(
                            "input_limit",
                            "Messages stop_reason exceeds byte limit",
                        ));
                    }
                    self.stop_reason = Some(reason.into());
                }
                if let Some(usage) = event.get("usage") {
                    self.take_usage(usage);
                }
            }
            Some("message_stop") => self.stopped = true,
            _ => self.unknown_events += 1,
        }
        Ok(out)
    }

    /// Build the completed (or incomplete) Responses-shaped response.
    pub(super) fn completed(&self) -> Result<Value, ModelError> {
        if !self.stopped {
            return Err(ModelError::new(
                "incomplete",
                "Messages stream ended before message_stop",
            ));
        }
        let reason = self
            .stop_reason
            .as_deref()
            .ok_or_else(|| protocol("Messages stream has no stop_reason"))?;
        let (status, complete) = match reason {
            "end_turn" | "stop_sequence" | "tool_use" => ("completed", true),
            "max_tokens" | "model_context_window_exceeded" => ("incomplete", false),
            other => {
                return Err(ModelError::new(
                    "protocol",
                    format!("Unhandled Messages stop_reason `{other}`; original response kept"),
                ));
            }
        };
        let mut output = Vec::new();
        let mut text = Vec::new();
        for block in self.blocks.values() {
            match block {
                Block::Text(body) => text.push(json!({"type":"output_text","text":body})),
                Block::Tool { id, name, json } => {
                    flush_text(&mut output, &mut text, status);
                    if id.is_empty() || name.is_empty() {
                        return Err(protocol("Incomplete Messages tool identity"));
                    }
                    let arguments = if json.is_empty() { "{}" } else { json };
                    if !serde_json::from_str::<Value>(arguments).is_ok_and(|v| v.is_object()) {
                        return Err(protocol("Messages tool arguments are not a JSON object"));
                    }
                    output.push(json!({"type":"function_call","status":status,"call_id":id,"name":name,"arguments":arguments}));
                }
                Block::Opaque { kind, block } => {
                    flush_text(&mut output, &mut text, status);
                    output.push(opaque::wrap(DIALECT, kind, block.clone()));
                }
            }
        }
        flush_text(&mut output, &mut text, status);
        if reason == "tool_use" && !output.iter().any(|item| item["type"] == "function_call") {
            return Err(protocol("Messages tool_use stop without a tool call"));
        }
        let mut raw = json!({
            "status":status,
            "output":output,
            "api":DIALECT,
            "stop_reason":reason,
            "messages":{"unknown_events":self.unknown_events,"complete":complete},
        });
        if let Some(id) = &self.id {
            raw["id"] = json!(id);
        }
        if let Some(usage) = self.usage() {
            raw["usage"] = usage;
        }
        Ok(raw)
    }
}

fn flush_text(output: &mut Vec<Value>, text: &mut Vec<Value>, status: &str) {
    if !text.is_empty() {
        output.push(
            json!({"type":"message","role":"assistant","status":status,"content":std::mem::take(text)}),
        );
    }
}

/// Fold a streamed delta into an opaque block: concatenate its string fields
/// onto the like-named fields (`thinking`, `signature`, ...). Never interpreted.
fn accumulate(block: &mut Value, delta: &Value, kind: &str) {
    let Some(delta) = delta.as_object() else {
        return;
    };
    let target = match kind {
        "thinking_delta" => Some(("thinking", "thinking")),
        "signature_delta" => Some(("signature", "signature")),
        _ => None,
    };
    for (key, value) in delta {
        if key == "type" {
            continue;
        }
        let field = match target {
            Some((from, to)) if key == from => to,
            _ => key.as_str(),
        };
        if let Some(piece) = value.as_str() {
            let current = block[field].as_str().unwrap_or_default().to_owned();
            block[field] = Value::String(current + piece);
        }
    }
}

/// A non-streaming JSON body on a streaming request is a provider error document.
pub(super) fn decode_json(raw: Value) -> Result<Value, ModelError> {
    if raw.get("error").is_some_and(|v| !v.is_null()) {
        return Err(provider_error(&raw));
    }
    Err(protocol("Messages response was not a stream"))
}

/// Map `{"type":"error","error":{"type","message"}}` onto the shared provider
/// error: `error.type` becomes the provider code.
pub(super) fn provider_error(raw: &Value) -> ModelError {
    let error = &raw["error"];
    ModelError::provider(
        None,
        &json!({"error":{"message":error["message"],"code":error["type"]}}),
    )
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn partial_counters_do_not_invent_input_totals_and_zero_is_known() {
        let mut stream = Messages::default();
        assert!(stream.usage().is_none());
        stream.take_usage(&json!({"input_tokens":7}));
        assert!(stream.usage().unwrap().get("input_tokens").is_none());
        stream.take_usage(&json!({"cache_read_input_tokens":2,"output_tokens":3}));
        let partial = stream.usage().unwrap();
        assert!(partial.get("input_tokens").is_none());
        assert_eq!(partial["input_tokens_details"]["cached_tokens"], 2);
        assert_eq!(partial["output_tokens"], 3);
        stream.take_usage(&json!({"cache_creation_input_tokens":1}));
        assert_eq!(stream.usage().unwrap()["input_tokens"], 10);

        let mut zero = Messages::default();
        zero.take_usage(&json!({"input_tokens":0,"cache_creation_input_tokens":0,
            "cache_read_input_tokens":0,"output_tokens":0}));
        assert_eq!(
            zero.usage().unwrap(),
            json!({"input_tokens":0,
            "input_tokens_details":{"cached_tokens":0},"output_tokens":0})
        );
    }

    #[test]
    fn summed_counters_are_checked_and_keep_independent_known_facts() {
        let max = i64::MAX as u64;
        for (input, creation, read, expected) in [
            (max, 0, 0, Some(max)),
            (max - 2, 1, 1, Some(max)),
            (max, 0, 1, None),
            (max, max, max, None),
        ] {
            let mut stream = Messages::default();
            stream.take_usage(&json!({"input_tokens":input,
                "cache_creation_input_tokens":creation,"cache_read_input_tokens":read,
                "output_tokens":5}));
            let usage = stream.usage().unwrap();
            assert_eq!(usage.get("input_tokens").and_then(Value::as_u64), expected);
            assert_eq!(usage["input_tokens_details"]["cached_tokens"], read);
            assert_eq!(usage["output_tokens"], 5);
        }
    }

    #[test]
    fn absent_delta_keeps_prior_usage_but_present_invalid_replaces_it_with_unknown() {
        let mut stream = Messages::default();
        stream.take_usage(&json!({"input_tokens":7,"cache_creation_input_tokens":1,
            "cache_read_input_tokens":2,"output_tokens":4}));
        let complete = stream.usage().unwrap();
        stream.take_usage(&json!({}));
        assert_eq!(stream.usage().unwrap(), complete);
        stream.take_usage(&json!({"output_tokens":null}));
        assert!(stream.usage().unwrap().get("output_tokens").is_none());
        for invalid in [
            json!(-1),
            json!(2.0),
            json!("2"),
            json!(u64::MAX),
            Value::Null,
        ] {
            stream.take_usage(&json!({"cache_read_input_tokens":2}));
            assert_eq!(stream.usage().unwrap()["input_tokens"], 10);
            stream.take_usage(&json!({"cache_read_input_tokens":invalid}));
            let unknown = stream.usage().unwrap();
            assert!(unknown.get("input_tokens").is_none());
            assert!(unknown.get("input_tokens_details").is_none());
        }
    }
}
