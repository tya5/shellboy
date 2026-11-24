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
    let mut complete = false;
    http_error_observed(response, context, &mut complete).await
}

pub(super) async fn http_error_observed(
    response: Response,
    context: &mut RequestContext<'_>,
    complete: &mut bool,
) -> ModelError {
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
            *complete = true;
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
    if !*complete {
        trim_unfinished_suffix(&mut body, context.secrets);
    }
    let mut error = match serde_json::from_slice::<Value>(&body) {
        Ok(raw) => ModelError::provider(Some(status), &raw),
        Err(_) => ModelError {
            kind: "http".into(),
            message: String::from_utf8_lossy(&body).into_owned(),
            provider_code: None,
            status: Some(status),
            transport_cause: None,
            http_observation: None,
            usage_limit: std::str::from_utf8(&body)
                .ok()
                .and_then(|text| {
                    crate::model::usage_limit::UsageLimit::text_for_status(
                        text,
                        chrono::Utc::now(),
                        Some(status),
                    )
                })
                .map(Box::new),
            stream_provider_status: None,
            strict_untyped_stream_error_before_done: false,
        },
    };
    if error.message.is_empty() {
        error.message = "Proxy returned an empty HTTP error body".into();
    }
    if truncated {
        error.usage_limit = None;
    }
    let mut error = redact(error, context.secrets);
    if truncated {
        const MARKER: &str = " [HTTP error body truncated]";
        error.message = error
            .message
            .chars()
            .take(MAX_ERROR_MESSAGE_CHARS - MARKER.len())
            .collect();
        error.message.push_str(MARKER);
    }
    error
}

fn trim_unfinished_suffix(body: &mut Vec<u8>, secrets: &[String]) {
    // The last bytes can be only a prefix of a known credential. Keep the
    // rest of the explanation and discard just the longest candidate suffix.
    let split = secrets
        .iter()
        .filter_map(|secret| {
            let secret = secret.as_bytes();
            (1..secret.len().min(body.len() + 1))
                .rev()
                .find(|&n| body.ends_with(&secret[..n]))
        })
        .max()
        .unwrap_or(0);
    body.truncate(body.len() - split);
    // An unfinished URL may end before its authority delimiter. It cannot be
    // safely parsed, so omit that last token rather than exposing it as a host.
    let last = body
        .iter()
        .rposition(|b| b.is_ascii_whitespace() || matches!(b, b'"' | b'\'' | b'<' | b'>' | b'`'))
        .map_or(0, |i| i + 1);
    if body[last..].windows(3).any(|w| w == b"://") {
        body.truncate(last);
    }
}
pub(super) fn register_secret(secrets: &mut Vec<String>, value: &str) {
    if !value.is_empty() {
        secrets.push(value.to_owned());
    }
    if let Some((scheme, token)) = value.split_once(' ')
        && (scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic"))
        && !token.trim().is_empty()
    {
        secrets.push(token.trim().to_owned());
    }
}

fn redact_text(text: &str, secrets: &[String]) -> String {
    let end_url = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`');
    let mut cleaned = String::with_capacity(text.len());
    for token in text.split_inclusive(end_url) {
        let word = token.strip_suffix(end_url).unwrap_or(token);
        let repaired = word.split_once("://").and_then(|(prefix, _)| {
            let start = prefix
                .rfind(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '+' | '-' | '.'))
                .map_or(0, |i| i + prefix[i..].chars().next().unwrap().len_utf8());
            let mut url = url::Url::parse(&word[start..]).ok()?;
            if url.host_str().is_none()
                || (url.username().is_empty() && url.password().is_none() && url.query().is_none())
            {
                return None;
            }
            let safe = if url.set_username("").is_err() || url.set_password(None).is_err() {
                "[redacted URL]"
            } else {
                url.set_query(None);
                url.as_str()
            };
            Some(format!("{}{safe}{}", &word[..start], &token[word.len()..]))
        });
        cleaned.push_str(repaired.as_deref().unwrap_or(token));
    }
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        if cleaned.contains(secret) {
            cleaned = cleaned.replace(secret, "[redacted]");
        }
    }
    cleaned
}

fn redact_value(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = redact_text(text, secrets),
        Value::Array(items) => items.iter_mut().for_each(|v| redact_value(v, secrets)),
        Value::Object(map) => {
            // Diagnostic keys can carry a reflected value too. Remove those
            // keys rather than merging unrelated entries under one placeholder.
            map.retain(|key, child| {
                if redact_text(key, secrets) != *key {
                    return false;
                }
                redact_value(child, secrets);
                true
            });
        }
        _ => {}
    }
}

/// Project failed request evidence while the request's credential values are
/// still available. Success and already persisted history are not rewritten.
pub(super) fn redact_failure(failure: &mut AttemptFailure, secrets: &[String]) {
    failure.error = redact(failure.error.clone(), secrets);
    // Partial output and completed items are conversation data. In particular,
    // the output-limit envelope itself can be adopted and replayed.
    if failure.error.kind != "output_limit"
        && let Some(raw) = &mut failure.raw_response
    {
        redact_value(raw, secrets);
    }
}

pub(super) fn redact(mut error: ModelError, secrets: &[String]) -> ModelError {
    error.message = redact_text(&error.message, secrets);
    if let Some(code) = &mut error.provider_code {
        *code = redact_text(code, secrets);
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
