//! Stateless wire adapters. Core consumes only the existing normalized response contract.
use super::*;
use crate::config::Api;
use std::ops::Deref;

mod chat;
mod responses;
use chat::Chat;

fn protocol(message: &str) -> ModelError {
    ModelError::new("protocol", message)
}

pub fn encode(api: Api, request: Value) -> Result<Value, ModelError> {
    match api {
        Api::Responses => responses::encode(request),
        Api::ChatCompletions => chat::encode(request),
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
        }
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ModelError> {
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
    pub fn finish(self, tools_enabled: bool) -> Result<ModelResponse, ModelError> {
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
