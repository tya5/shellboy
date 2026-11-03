//! Chat Completions wire adapter (moved verbatim from `dialect.rs`).
use super::*;

pub(super) fn encode(
    request: Value,
    tags: Option<&[ItemTag]>,
) -> Result<(Value, Labels), ModelError> {
    let input = request["input"]
        .as_array()
        .ok_or_else(|| protocol("Conversation input must be an array"))?;
    let mut messages: Vec<Value> = Vec::new();
    for (index, item) in input.iter().enumerate() {
        // The item's source (#186); a merged message keeps each part's own.
        let tag = tags.and_then(|tags| tags.get(index));
        let category = tag.map(|tag| tag.category);
        match item["type"].as_str() {
            // Responses reasoning and other dialects' opaque blocks are not replayed.
            Some("reasoning") | Some(crate::model::opaque::TYPE) => continue,
            Some("function_call") => {
                let mut call = json!({"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":item["arguments"]}});
                wire_bytes::mark(&mut call, category);
                if !messages
                    .last()
                    .is_some_and(|last| last["role"] == "assistant")
                {
                    let mut message = json!({"role":"assistant","content":null,"tool_calls":[]});
                    wire_bytes::mark(&mut message, category);
                    messages.push(message);
                }
                let last = messages.last_mut().expect("assistant message");
                if last.get("tool_calls").is_none() {
                    last["tool_calls"] = json!([]);
                }
                last["tool_calls"]
                    .as_array_mut()
                    .expect("adapter array")
                    .push(call);
            }
            Some("function_call_output") => {
                let output = item["output"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| item["output"].to_string());
                let mut message =
                    json!({"role":"tool","tool_call_id":item["call_id"],"content":output});
                wire_bytes::mark(&mut message, category);
                messages.push(message);
            }
            Some("message") | None => {
                let role = match item["role"].as_str() {
                    Some("developer" | "system") => "system",
                    Some("user") => "user",
                    Some("assistant") => "assistant",
                    _ => return Err(protocol("Unsupported conversation message role")),
                };
                let content = if let Some(text) = item["content"].as_str() {
                    json!(text)
                } else {
                    let parts = item["content"]
                        .as_array()
                        .ok_or_else(|| protocol("Message content must be text or an array"))?;
                    let mut mapped = Vec::new();
                    for (index, part) in parts.iter().enumerate() {
                        match part["type"].as_str() {
                            Some("input_text" | "output_text") => {
                                mapped.push(json!({"type":"text","text":part["text"]}))
                            }
                            Some("refusal") => {
                                mapped.push(json!({"type":"text","text":part["refusal"]}))
                            }
                            Some("input_image") => {
                                let mut image = json!({"url":part["image_url"]});
                                if let Some(detail) = part.get("detail") {
                                    image["detail"] = detail.clone();
                                }
                                mapped.push(json!({"type":"image_url","image_url":image}));
                            }
                            _ => {
                                return Err(ModelError::new(
                                    "unsupported_item",
                                    "Unsupported Chat input content; no request sent",
                                ));
                            }
                        }
                        if let Some(last) = mapped.last_mut() {
                            wire_bytes::mark(last, tag.map(|tag| tag.part(index)));
                        }
                    }
                    json!(mapped)
                };
                if role == "assistant"
                    && messages
                        .last()
                        .is_some_and(|message| message["role"] == "assistant")
                {
                    let last = messages.last_mut().expect("existing assistant");
                    let owner = wire_bytes::marked(last);
                    let previous = last["content"].take();
                    let parts =
                        |value: Value, source: Option<wire_bytes::Category>| -> Vec<Value> {
                            match value {
                                Value::Null => vec![],
                                Value::Array(parts) => parts,
                                value => {
                                    let mut part = json!({"type":"text","text":value});
                                    wire_bytes::mark(&mut part, source);
                                    vec![part]
                                }
                            }
                        };
                    let mut combined = parts(previous, owner);
                    combined.extend(parts(content, category));
                    last["content"] = json!(combined);
                } else {
                    let mut message = json!({"role":role,"content":content});
                    wire_bytes::mark(&mut message, category);
                    messages.push(message);
                }
            }
            _ => {
                return Err(ModelError::new(
                    "unsupported_item",
                    "Unsupported Chat replay item; no request sent",
                ));
            }
        }
    }
    let mut result = json!({"model":request["model"],"messages":messages,"stream":true,"stream_options":{"include_usage":true}});
    if let Some(effort) = request.pointer("/reasoning/effort") {
        result["reasoning_effort"] = effort.clone();
    }
    if let Some(tools) = request["tools"]
        .as_array()
        .filter(|tools| !tools.is_empty())
    {
        let functions = tools
            .iter()
            .map(|tool| {
                let mut function = tool
                    .as_object()
                    .cloned()
                    .ok_or_else(|| protocol("Function definition must be an object"))?;
                if function
                    .remove("type")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .as_deref()
                    != Some("function")
                {
                    return Err(protocol("Unsupported tool definition"));
                }
                Ok(json!({"type":"function","function":function}))
            })
            .collect::<Result<Vec<_>, ModelError>>()?;
        result["tools"] = json!(functions);
    }
    let labels = if tags.is_some() {
        wire_bytes::extract(&mut result)
    } else {
        Labels::new()
    };
    Ok((result, labels))
}

#[derive(Default)]
struct Call {
    id: String,
    id_reported: bool,
    name: String,
    arguments: String,
    repeated_full_names: u64,
}
#[derive(Default)]
pub(super) struct Chat {
    pub(super) decoder: SseDecoder,
    id: Option<String>,
    content: Vec<Value>,
    calls: BTreeMap<u64, Call>,
    pub(super) finish: Option<String>,
    pub(super) usage: Option<Value>,
    pub(super) usage_chunks_without_choices: u64,
}
impl Chat {
    fn body(&mut self, kind: &str, field: &str, text: &str) {
        if let Some(last) = self.content.last_mut()
            && last["type"] == kind
            && let Some(Value::String(body)) = last.get_mut(field)
        {
            body.push_str(text);
            return;
        }
        self.content.push(json!({"type":kind,field:text}));
    }
    pub(super) fn item(&mut self, value: &Value) -> Result<Vec<Value>, ModelError> {
        if let Some(value) = value.get("usage").filter(|v| !v.is_null()) {
            self.usage = Some(usage(value));
        }
        if value.get("error").is_some_and(|v| !v.is_null()) {
            return Ok(vec![json!({"type":"error","error":value["error"]})]);
        }
        if let Some(id) = value["id"].as_str() {
            if id.len() > 1024 {
                return Err(ModelError::new(
                    "input_limit",
                    "Chat response id exceeds byte limit",
                ));
            }
            if self.id.as_deref().is_some_and(|old| old != id) {
                return Err(protocol("Chat response ID changed"));
            }
            self.id = Some(id.into());
        }
        let no_choices = Vec::new();
        let choices = match value.get("choices") {
            Some(value) => value
                .as_array()
                .ok_or_else(|| protocol("Chat choices must be an array"))?,
            None if value.get("usage").is_some_and(|usage| !usage.is_null()) => {
                self.usage_chunks_without_choices += 1;
                &no_choices
            }
            None => return Err(protocol("Chat chunk has no choices array or usage")),
        };
        if choices.len() > 1 {
            return Err(protocol("Multiple Chat choices are unsupported"));
        }
        let mut events = Vec::new();
        for choice in choices {
            if choice["index"].as_u64() != Some(0) {
                return Err(protocol("Only Chat choice zero is supported"));
            }
            let delta = &choice["delta"];
            if !delta.is_null() && !delta.is_object() {
                return Err(protocol("Chat delta must be an object"));
            }
            if delta
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| role != "assistant")
            {
                return Err(protocol("Chat response role is not assistant"));
            }
            if self.finish.is_some() && delta.as_object().is_some_and(|v| !v.is_empty()) {
                return Err(protocol("Chat output after finish_reason"));
            }
            if let Some(text) = delta["content"].as_str() {
                self.body("output_text", "text", text);
                if !text.is_empty() {
                    events.push(json!({"type":"response.output_text.delta","delta":text}));
                }
            } else if delta.get("content").is_some_and(|v| !v.is_null()) {
                return Err(protocol("Unsupported Chat content delta"));
            }
            if let Some(refusal) = delta["refusal"].as_str() {
                self.body("refusal", "refusal", refusal);
                events.push(json!({"type":"response.refusal.delta","delta":refusal}));
            }
            if let Some(calls) = delta.get("tool_calls").filter(|v| !v.is_null()) {
                for call in calls
                    .as_array()
                    .ok_or_else(|| protocol("Chat tool_calls must be an array"))?
                {
                    let index = call["index"]
                        .as_u64()
                        .filter(|i| *i < 1024)
                        .ok_or_else(|| protocol("Invalid Chat tool index"))?;
                    if call
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| kind != "function")
                    {
                        return Err(protocol("Unsupported Chat tool kind"));
                    }
                    let entry = self.calls.entry(index).or_default();
                    if call
                        .get("id")
                        .is_some_and(|id| !id.is_null() && !id.is_string())
                    {
                        return Err(protocol("Chat tool id must be a string"));
                    }
                    if let Some(id) = call["id"].as_str() {
                        if !entry.id.is_empty() && entry.id != id {
                            return Err(protocol("Chat tool ID changed"));
                        }
                        if id.len() > 1024 {
                            return Err(protocol("Chat tool ID too large"));
                        }
                        entry.id = id.into();
                        entry.id_reported = true;
                    }
                    if let Some(name) = call["function"]["name"].as_str() {
                        if entry.name == "shell" && name == entry.name {
                            entry.repeated_full_names += 1;
                        } else {
                            entry.name.push_str(name);
                        }
                        if entry.name.len() > 1024 {
                            return Err(protocol("Chat function name too large"));
                        }
                    }
                    if call["function"]
                        .get("arguments")
                        .is_some_and(|value| !value.is_null() && !value.is_string())
                    {
                        return Err(protocol("Chat arguments delta must be a string"));
                    }
                    if let Some(arguments) = call["function"]["arguments"].as_str() {
                        if entry.arguments.len().saturating_add(arguments.len())
                            > MAX_TOOL_ARGUMENT_BYTES
                        {
                            return Err(ModelError::new(
                                "input_limit",
                                "Chat tool arguments exceed byte limit",
                            ));
                        }
                        entry.arguments.push_str(arguments);
                        events.push(json!({"type":"response.function_call_arguments.delta","delta":arguments}));
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                if reason.len() > 64 {
                    return Err(ModelError::new(
                        "input_limit",
                        "Chat finish reason exceeds byte limit",
                    ));
                }
                if self.finish.is_some() {
                    return Err(protocol("Duplicate Chat finish_reason"));
                }
                self.finish = Some(reason.into());
            }
        }
        Ok(events)
    }
    pub(super) fn completed(&self) -> Result<Value, ModelError> {
        let reason = self
            .finish
            .as_deref()
            .ok_or_else(|| protocol("Chat DONE without finish_reason"))?;
        if reason == "tool_calls" && self.calls.is_empty() {
            return Err(protocol("Chat tool finish without calls"));
        }
        let stop_with_calls = reason == "stop" && !self.calls.is_empty();
        if stop_with_calls
            && self.calls.values().any(|call| {
                !serde_json::from_str::<Value>(&call.arguments).is_ok_and(|value| value.is_object())
            })
        {
            return Err(protocol("Chat stop with incomplete tool arguments"));
        }
        let id_seed = if self.calls.values().any(|call| call.id.is_empty()) {
            let response_id = self
                .id
                .as_deref()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| protocol("Cannot synthesize a tool id without a response id"))?;
            Some(crate::digest::sha256_hex(response_id.as_bytes()))
        } else {
            None
        };
        let facts = json!({"response_id":self.id,"finish_reason":reason,"stop_with_complete_tool_calls":stop_with_calls,
            "usage_chunks_without_choices":self.usage_chunks_without_choices});
        let complete = matches!(reason, "stop" | "tool_calls");
        let status = if complete { "completed" } else { "incomplete" };
        let mut output = Vec::new();
        if !self.content.is_empty()
            || (self.calls.is_empty() && self.usage_chunks_without_choices > 0)
        {
            output.push(
                json!({"type":"message","role":"assistant","status":status,"content":self.content,"chat_completions":facts}),
            );
        }
        for (index, call) in &self.calls {
            if call.name.is_empty() {
                return Err(protocol("Incomplete Chat tool identity"));
            }
            let synthesized = call.id.is_empty();
            let id = if synthesized {
                let mut seed = id_seed
                    .as_ref()
                    .expect("missing-id seed")
                    .as_bytes()
                    .to_vec();
                seed.extend_from_slice(&index.to_be_bytes());
                format!("call_sb_{}", &crate::digest::sha256_hex(seed)[..48])
            } else {
                call.id.clone()
            };
            let mut call_facts = facts.clone();
            call_facts["index"] = json!(index);
            call_facts["id_synthesized"] = json!(synthesized);
            call_facts["original_id"] = if call.id_reported {
                json!(call.id)
            } else {
                Value::Null
            };
            call_facts["id_reported"] = json!(call.id_reported);
            call_facts["id_source"] = json!(if synthesized {
                "sha256_response_id_index"
            } else {
                "provider"
            });
            call_facts["repeated_full_name_deltas"] = json!(call.repeated_full_names);
            output.push(json!({"type":"function_call","status":status,"call_id":id,"name":call.name,"arguments":call.arguments,"chat_completions":call_facts}));
        }
        let mut raw = json!({"status":if matches!(reason,"stop"|"tool_calls"){"completed"}else{"incomplete"},"output":output,"api":"chat_completions","finish_reason":reason,"chat_completions":facts});
        if let Some(id) = &self.id {
            raw["id"] = json!(id);
        }
        if let Some(usage) = &self.usage {
            raw["usage"] = usage.clone();
        }
        Ok(raw)
    }
}

pub(super) fn decode_json(raw: Value) -> Result<Value, ModelError> {
    if raw.get("error").is_some_and(|v| !v.is_null()) {
        return Err(ModelError::provider(None, &raw));
    }
    let choices = raw["choices"]
        .as_array()
        .filter(|v| v.len() == 1)
        .ok_or_else(|| protocol("Chat JSON requires one choice"))?;
    let choice = &choices[0];
    let message = &choice["message"];
    if choice["index"].as_u64() != Some(0) || !message.is_object() || message["role"] != "assistant"
    {
        return Err(protocol("Invalid Chat JSON assistant choice"));
    }
    let mut delta = message.clone();
    if let Some(calls) = delta.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for (index, call) in calls.iter_mut().enumerate() {
            call["index"] = json!(index);
        }
    }
    let mut chat = Chat::default();
    chat.item(&json!({"id":raw["id"],"choices":[{"index":0,"delta":delta,"finish_reason":choice["finish_reason"]}],"usage":raw["usage"]}))?;
    chat.completed()
}
