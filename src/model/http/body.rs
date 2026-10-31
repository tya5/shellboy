//! Shared bounded JSON/error body handling; endpoint-specific data stays with callers.
use super::*;
use crate::model::MAX_ERROR_MESSAGE_CHARS;
use futures_util::StreamExt;
const MAX_ERROR_BYTES: usize = 64 * 1024;

pub(super) struct JsonBody {
    pub(super) raw: Value,
    pub(super) provider_error: Option<ModelError>,
}
impl JsonBody {
    pub(super) fn into_result(self) -> Result<Value, ModelError> {
        match self.provider_error {
            Some(error) => Err(error),
            None => Ok(self.raw),
        }
    }
}
pub(super) async fn read_json(
    response: Response,
    max: usize,
    malformed: &str,
    context: &mut RequestContext<'_>,
    facts: Option<&mut ResponseFacts>,
) -> Result<JsonBody, ModelError> {
    let status = response.status();
    if !status.is_success() {
        return Err(http_error(response, context).await);
    }
    let bytes = bounded_body(response, max, context, facts).await?;
    let raw: Value =
        serde_json::from_slice(&bytes).map_err(|_| ModelError::new("protocol", malformed))?;
    let provider_error = raw.get("error").filter(|error| !error.is_null()).map(|_| {
        redact(
            ModelError::provider(Some(status.as_u16()), &raw),
            context.secrets,
        )
    });
    Ok(JsonBody {
        raw,
        provider_error,
    })
}

async fn bounded_body(
    response: Response,
    max: usize,
    context: &mut RequestContext<'_>,
    mut facts: Option<&mut ResponseFacts>,
) -> Result<Vec<u8>, ModelError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let next = context
            .deadline
            .wait(context.cancel, "receiving response body", stream.next())
            .await?;
        let Some(chunk) = next else {
            return Ok(body);
        };
        let chunk = chunk.map_err(|error| transport_error(error, "receiving response body"))?;
        if !chunk.is_empty() {
            context.deadline.received();
            if let Some(facts) = &mut facts {
                facts.received = ResponseReceived::Output;
            }
            if let Some(events) = context.events {
                events.received(context.deadline.received_at());
            }
        }
        if chunk.len() > max.saturating_sub(body.len()) {
            return Err(ModelError::new(
                "input_limit",
                "HTTP response body exceeded byte limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
}
pub(super) async fn http_error(response: Response, context: &mut RequestContext<'_>) -> ModelError {
    let status = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    let mut truncated = false;
    loop {
        let next = match context
            .deadline
            .wait(context.cancel, "receiving HTTP error body", stream.next())
            .await
        {
            Ok(next) => next,
            Err(mut error) => {
                error.status = Some(status);
                return error;
            }
        };
        let Some(chunk) = next else {
            break;
        };
        let Ok(chunk) = chunk else {
            break;
        };
        if !chunk.is_empty() {
            context.deadline.received();
            if let Some(events) = context.events {
                events.received(context.deadline.received_at());
            }
        }
        let take = chunk.len().min(MAX_ERROR_BYTES - body.len());
        body.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            truncated = true;
            break;
        }
    }
    let mut error = match serde_json::from_slice::<Value>(&body) {
        Ok(raw) => ModelError::provider(Some(status), &raw),
        Err(_) => ModelError {
            kind: "http".into(),
            message: String::from_utf8_lossy(&body)
                .chars()
                .take(MAX_ERROR_MESSAGE_CHARS)
                .collect(),
            provider_code: None,
            status: Some(status),
            http_observation: None,
            usage_limit: std::str::from_utf8(&body)
                .ok()
                .and_then(|text| {
                    crate::model::usage_limit::UsageLimit::text(text, chrono::Utc::now())
                })
                .map(Box::new),
        },
    };
    if error.message.is_empty() {
        error.message = "Proxy returned an empty HTTP error body".into();
    }
    if truncated {
        error.usage_limit = None;
        error.message.push_str(" [HTTP error body truncated]");
    }
    redact(error, context.secrets)
}
pub(super) fn redact(mut error: ModelError, secrets: &[String]) -> ModelError {
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        error.message = error.message.replace(secret, "[redacted]");
        if let Some(code) = &mut error.provider_code {
            *code = code.replace(secret, "[redacted]");
        }
    }
    if error.message.chars().count() > MAX_ERROR_MESSAGE_CHARS {
        error.message = error
            .message
            .chars()
            .take(MAX_ERROR_MESSAGE_CHARS - 34)
            .collect::<String>()
            + " [diagnostic truncated]";
    }
    error.provider_code = error
        .provider_code
        .map(|code| code.chars().take(256).collect());
    error
}
