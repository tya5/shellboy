//! Stateless wire adapters. Core consumes only the existing normalized response contract.
use super::*;
use crate::config::Api;
use crate::model::wire_bytes::{self, ItemTag, Labels};
use std::ops::Deref;

mod chat;
mod messages;
mod responses;
use chat::Chat;
pub use messages::ANTHROPIC_VERSION;
use messages::Messages;

fn protocol(message: &str) -> ModelError {
    ModelError::new("protocol", message)
}

pub fn encode(api: Api, request: Value) -> Result<Value, ModelError> {
    encode_with(api, request, crate::config::DEFAULT_MESSAGES_MAX_TOKENS)
}
/// `max_tokens` is used only by `anthropic_messages`, which requires it.
/// Sends no cache hint; see [`encode_hinted`].
pub fn encode_with(api: Api, request: Value, max_tokens: u64) -> Result<Value, ModelError> {
    encode_hinted(api, request, max_tokens, None)
}
/// Encode with an optional common cache hint (#127). `None` (the connection's
/// `prompt_caching = false`) sends nothing cache-related.
pub fn encode_hinted(
    api: Api,
    request: Value,
    max_tokens: u64,
    cache: Option<CacheHint>,
) -> Result<Value, ModelError> {
    // Preserve the legacy Messages-only argument on this public adapter.
    let configured = (api == Api::AnthropicMessages).then_some(max_tokens);
    encode_tagged(api, request, configured, cache, None).map(|(wire, _)| wire)
}
/// Encode, carrying each canonical input item's source tag (#186) onto the
/// wire fragments built from it. With `tags`, also return the labels that
/// partition the body; the body itself is identical to the untagged encoding.
/// `tags` must have one entry per `input` item.
pub(crate) fn encode_tagged(
    api: Api,
    request: Value,
    max_tokens: Option<u64>,
    cache: Option<CacheHint>,
    tags: Option<&[ItemTag]>,
) -> Result<(Value, Labels), ModelError> {
    let (mut wire, labels) = match api {
        // Responses providers cache prefixes on their own; the existing
        // `prompt_cache_key`/session handling is the only routing hint. Chat
        // has no cache field. Neither sends anything new for the hint.
        Api::Responses => responses::encode(request, tags),
        Api::ChatCompletions => chat::encode(request, tags),
        Api::AnthropicMessages => messages::encode(
            request,
            max_tokens.unwrap_or(crate::config::DEFAULT_MESSAGES_MAX_TOKENS),
            cache,
            tags,
        ),
    }?;
    if let Some(limit) = max_tokens {
        let key = match api {
            Api::Responses => "max_output_tokens",
            Api::ChatCompletions => "max_completion_tokens",
            Api::AnthropicMessages => "max_tokens",
        };
        wire[key] = json!(limit);
    }
    Ok((wire, labels))
}

/// The common cache hint (#127): structure only Shellboy knows, which a
/// dialect encoder turns into its own cache markers. Indices are into the
/// canonical (Responses-shaped) `input`, taken before `context_role` mapping.
///
/// - `input[..prefix_items]` are the leading system/developer instructions.
///   With the tools they form the stable prefix.
/// - `input[prefix_items..conversation_items]` is the growing conversation,
///   whose end is the conversation tail.
/// - `input[conversation_items..]` is the trailing run of system/developer
///   items: per-request attachment notices or a summary prompt, or saved
///   notices that happen to be last (an event, or the `Shellboy context v1`
///   line saved at a Turn start or after a change, #183). The breakpoint stays
///   before them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheHint {
    pub prefix_items: usize,
    pub conversation_items: usize,
}
impl CacheHint {
    /// Derive the hint from a canonical request.
    pub fn of(request: &Value) -> Self {
        let empty = Vec::new();
        let input = request["input"].as_array().unwrap_or(&empty);
        let inserted = |item: &Value| {
            matches!(item["type"].as_str(), Some("message") | None)
                && matches!(item["role"].as_str(), Some("system" | "developer"))
        };
        let prefix_items = input.iter().take_while(|item| inserted(item)).count();
        let trailing = input[prefix_items..]
            .iter()
            .rev()
            .take_while(|item| inserted(item))
            .count();
        Self {
            prefix_items,
            conversation_items: input.len() - trailing,
        }
    }
}

pub(super) fn usage(value: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for (source, target) in [
        ("prompt_tokens", "input_tokens"),
        ("completion_tokens", "output_tokens"),
        ("total_tokens", "total_tokens"),
    ] {
        if let Some(value) = value.get(source) {
            out.insert(target.into(), value.clone());
        }
    }
    for (source, target) in [
        ("prompt_tokens_details", "input_tokens_details"),
        ("completion_tokens_details", "output_tokens_details"),
    ] {
        if let Some(value) = value.get(source) {
            out.insert(target.into(), value.clone());
        }
    }
    crate::usage::StandardUsage::from_value(&Value::Object(out)).to_value()
}

/// Wrap the existing canonical stream, preserving its partial facts and validators.
pub struct Stream {
    inner: ResponseStream,
    chat: Option<Chat>,
    messages: Option<Messages>,
}
impl Deref for Stream {
    type Target = ResponseStream;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl Stream {
    pub fn new(api: Api) -> Self {
        Self {
            inner: ResponseStream::default(),
            chat: (api == Api::ChatCompletions).then(Chat::default),
            messages: (api == Api::AnthropicMessages).then(Messages::default),
        }
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
        if self.messages.is_some() {
            return self.push_messages(bytes);
        }
        let Some(chat) = &mut self.chat else {
            return self.inner.push(bytes);
        };
        if self.inner.is_terminal() {
            return Ok(vec![]);
        }
        let (chunks, error) = match chat.decoder.decode(
            bytes,
            |value| value.get("error").is_some_and(|error| !error.is_null()),
            true,
        ) {
            Ok(chunks) => (chunks, None),
            Err(failure) => (failure.events, Some(failure.error)),
        };
        let mut events = Vec::new();
        for chunk in chunks {
            let converted = chat.item(&chunk);
            if let Some(usage) = &chat.usage {
                // Usage can arrive before DONE; retain reported facts if transport ends.
                self.inner.failed_response = Some(
                    json!({"status":"incomplete","api":"chat_completions","output":[],"usage":usage}),
                );
            }
            for event in converted? {
                let wire = format!("data: {}\n\n", event);
                events.extend(self.inner.push(wire.as_bytes())?);
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
        if chat.decoder.done_markers > 0 && !self.inner.is_terminal() {
            let raw = match chat.completed() {
                Ok(raw) => raw,
                Err(error) => {
                    self.inner.terminal = Some(Err(error.clone()));
                    return Err(error);
                }
            };
            self.inner.failed_response = None;
            let kind = if raw["status"] == "completed" {
                "response.completed"
            } else {
                "response.incomplete"
            };
            events.extend(
                self.inner.push(
                    format!("data: {}\n\n", json!({"type":kind,"response":raw})).as_bytes(),
                )?,
            );
        }
        Ok(events)
    }
    fn push_messages(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
        let messages = self.messages.as_mut().expect("messages stream");
        if self.inner.is_terminal() {
            return Ok(vec![]);
        }
        let (chunks, error) = match messages.decoder.decode(
            bytes,
            |value| value["type"] == "error" || value["type"] == "message_stop",
            true,
        ) {
            Ok(chunks) => (chunks, None),
            Err(failure) => (failure.events, Some(failure.error)),
        };
        let mut events = Vec::new();
        for chunk in chunks {
            let converted = messages.item(&chunk);
            if let Some(usage) = messages.usage() {
                // Usage can arrive before the end; keep reported facts if the transport ends.
                self.inner.failed_response = Some(
                    json!({"status":"incomplete","api":"anthropic_messages","output":[],"usage":usage}),
                );
            }
            for event in converted? {
                let wire = format!("data: {}\n\n", event);
                events.extend(self.inner.push(wire.as_bytes())?);
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
        if messages.stopped && !self.inner.is_terminal() {
            let raw = match messages.completed() {
                Ok(raw) => raw,
                Err(error) => {
                    self.inner.terminal = Some(Err(error.clone()));
                    return Err(error);
                }
            };
            self.inner.failed_response = None;
            let kind = if raw["status"] == "completed" {
                "response.completed"
            } else {
                "response.incomplete"
            };
            events.extend(
                self.inner.push(
                    format!("data: {}\n\n", json!({"type":kind,"response":raw})).as_bytes(),
                )?,
            );
        }
        Ok(events)
    }
    pub fn finish(self, tools_enabled: bool) -> Result<ModelResponse, ModelError> {
        if self
            .messages
            .as_ref()
            .is_some_and(|messages| !messages.decoder.is_at_boundary())
        {
            return Err(ModelError::new(
                "incomplete",
                "EOF in partial Messages SSE frame",
            ));
        }
        if self
            .chat
            .as_ref()
            .is_some_and(|chat| !chat.decoder.is_at_boundary())
        {
            return Err(ModelError::new(
                "incomplete",
                "EOF in partial Chat SSE frame",
            ));
        }
        let reason = self.chat.as_ref().and_then(|chat| chat.finish.clone());
        self.inner.finish(tools_enabled).map_err(|mut error| {
            if let Some(reason) = reason {
                error
                    .message
                    .push_str(&format!("; Chat finish_reason={reason}"));
            }
            error
        })
    }
}

pub fn decode_json(api: Api, raw: Value) -> Result<Value, ModelError> {
    match api {
        Api::Responses => responses::decode_json(raw),
        Api::ChatCompletions => chat::decode_json(raw),
        // Messages requests always stream, so a JSON body is a provider error
        // document, never a message.
        Api::AnthropicMessages => messages::decode_json(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::opaque;

    fn conversation() -> Value {
        json!({"model":"m","input":[
            {"role":"user","content":[{"type":"input_text","text":"hi"}]},
            opaque::wrap("anthropic_messages","thinking",json!({"type":"thinking","thinking":"t","signature":"s"})),
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}
        ]})
    }

    #[test]
    fn responses_and_chat_skip_other_dialects_opaque_items_and_keep_the_rest() {
        let responses = encode(Api::Responses, conversation()).unwrap();
        let items = responses["input"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| !opaque::is_opaque(item)));
        assert_eq!(items[1]["role"], "assistant");
        let chat = encode(Api::ChatCompletions, conversation()).unwrap();
        let messages = chat["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(!chat.to_string().contains("signature"));
    }

    #[test]
    fn a_conversation_without_opaque_items_is_unchanged_for_responses() {
        let plain = json!({"model":"m","input":[{"role":"user","content":"x"}]});
        assert_eq!(encode(Api::Responses, plain.clone()).unwrap(), plain);
    }
}
