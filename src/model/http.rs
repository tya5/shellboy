//! Native asynchronous HTTP transport. No request retry or alternate API fallback.
use super::{MAX_RESPONSE_BYTES, ModelError, ModelResponse, ResponseStream};
use crate::config::{ApiEndpoint, Config, Connection};
use futures_util::StreamExt;
use reqwest::{
    Client, Response,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_EMBEDDING_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODEL_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_MODEL_CATALOG_ENTRIES: usize = 10_000;
const MAX_MODEL_ID_BYTES: usize = 1024;
const MODEL_CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug)]
pub struct AttemptFailure {
    pub error: ModelError,
    pub partial_text: String,
    pub completed_items: BTreeMap<u64, Value>,
    pub raw_response: Option<Value>,
}
impl From<ModelError> for AttemptFailure {
    fn from(error: ModelError) -> Self {
        Self {
            error,
            partial_text: String::new(),
            completed_items: BTreeMap::new(),
            raw_response: None,
        }
    }
}
#[derive(Debug)]
pub struct EmbeddingResponse {
    /// Vectors in the same order as input, independent of provider data order.
    pub vectors: Vec<Vec<f32>>,
    pub dimension: usize,
    pub usage: Option<Value>,
}
#[derive(Clone)]
pub struct HttpModelClient {
    client: Client,
    connection: Connection,
}
impl HttpModelClient {
    pub fn new(connection: Connection) -> Result<Self, ModelError> {
        let mut config = Config::default();
        config
            .connections
            .insert("selected".into(), connection.clone());
        config
            .validate()
            .map_err(|e| ModelError::new("config_invalid", e.to_string()))?;
        let client = Client::builder()
            .tls_danger_accept_invalid_certs(!connection.ssl_verify)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| ModelError::new("transport", "Unable to initialize HTTP client"))?;
        Ok(Self { client, connection })
    }
    fn headers(&self) -> Result<(HeaderMap, Vec<String>), ModelError> {
        let credentials = self
            .connection
            .credentials(|name| std::env::var(name).ok())
            .map_err(|e| ModelError::new("config_missing", e.to_string()))?;
        let mut headers = HeaderMap::new();
        let mut secrets = Vec::new();
        for (name, value) in credentials.headers() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ModelError::new("config_invalid", "Invalid configured header name"))?;
            let mut header = HeaderValue::from_str(value).map_err(|_| {
                ModelError::new("config_invalid", "Invalid configured header value")
            })?;
            header.set_sensitive(true);
            headers.insert(name, header);
            if !value.is_empty() {
                secrets.push(value.clone());
                if let Some(key) = value.strip_prefix("Bearer ") {
                    secrets.push(key.into());
                }
            }
        }
        secrets.push(self.connection.base_url.clone());
        Ok((headers, secrets))
    }
    async fn send(
        &self,
        endpoint: ApiEndpoint,
        request: &Value,
        cancel: &CancellationToken,
    ) -> Result<(Response, Vec<String>), ModelError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let (headers, secrets) = self.headers()?;
        let endpoint_kind = endpoint;
        let endpoint = self
            .connection
            .endpoint(endpoint)
            .map_err(|e| ModelError::new("config_invalid", e.to_string()))?;
        let accept = match endpoint_kind {
            ApiEndpoint::Responses => "text/event-stream, application/json",
            ApiEndpoint::Embeddings | ApiEndpoint::Models => "application/json",
        };
        let request = self
            .client
            .post(endpoint)
            .headers(headers)
            .header(reqwest::header::ACCEPT, accept)
            .json(request);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            response = request.send() => response.map(|r| (r, secrets)).map_err(|_| ModelError::new("transport", "HTTP request failed before response headers; inspect proxy connectivity and TLS settings")),
        }
    }
    /// Fetch the configured proxy's catalog without embedding a product-specific model list.
    /// The deadline includes both response headers and the complete bounded body.
    pub async fn models(&self, cancel: CancellationToken) -> Result<Vec<String>, ModelError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            result = tokio::time::timeout(MODEL_CATALOG_TIMEOUT, self.models_request(&cancel)) => {
                result.unwrap_or_else(|_| Err(ModelError::new("timeout", "Model list request timed out after 20 seconds")))
            }
        }
    }
    async fn models_request(&self, cancel: &CancellationToken) -> Result<Vec<String>, ModelError> {
        let (headers, secrets) = self.headers()?;
        let endpoint = self
            .connection
            .endpoint(ApiEndpoint::Models)
            .map_err(|e| ModelError::new("config_invalid", e.to_string()))?;
        let response = self
            .client
            .get(endpoint)
            .headers(headers)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| {
                ModelError::new(
                    "transport",
                    "Unable to retrieve model list; inspect proxy connectivity and TLS settings",
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_error(response, cancel, &secrets).await);
        }
        let bytes = bounded_body(response, MAX_MODEL_CATALOG_BYTES, cancel).await?;
        let raw: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ModelError::new("protocol", "Proxy returned malformed model list JSON"))?;
        if raw.get("error").is_some_and(|e| !e.is_null()) {
            return Err(redact(
                ModelError::provider(Some(status.as_u16()), &raw),
                &secrets,
            ));
        }
        let data = raw
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| ModelError::new("protocol", "Model list response has no data array"))?;
        if data.len() > MAX_MODEL_CATALOG_ENTRIES {
            return Err(ModelError::new(
                "input_limit",
                "Model list exceeded entry limit",
            ));
        }
        let mut models = Vec::with_capacity(data.len());
        for item in data {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= MAX_MODEL_ID_BYTES
                        && !id.chars().any(|c| c.is_whitespace() || c.is_control())
                })
                .ok_or_else(|| {
                    ModelError::new("protocol", "Model list contains an invalid model ID")
                })?;
            models.push(id.to_owned());
        }
        models.sort_unstable();
        models.dedup();
        Ok(models)
    }
    // Recovery needs the bounded partial text, items, and original response together.
    #[allow(clippy::result_large_err)]
    pub async fn response(
        &self,
        mut request: Value,
        cancel: CancellationToken,
        events: Option<mpsc::Sender<Value>>,
    ) -> Result<ModelResponse, AttemptFailure> {
        let Some(object) = request.as_object_mut() else {
            return Err(ModelError::new("protocol", "Responses request must be an object").into());
        };
        object.insert("stream".into(), Value::Bool(true));
        object.insert("store".into(), Value::Bool(false));
        let tools_enabled = request
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty());
        let (response, secrets) = self.send(ApiEndpoint::Responses, &request, &cancel).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_error(response, &cancel, &secrets).await.into());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if content_type == "application/json" {
            let bytes = bounded_body(response, MAX_RESPONSE_BYTES, &cancel).await?;
            let raw: Value = serde_json::from_slice(&bytes)
                .map_err(|_| ModelError::new("protocol", "Proxy returned malformed JSON"))?;
            let result = if raw.get("error").is_some_and(|e| !e.is_null()) {
                Err(redact(
                    ModelError::provider(Some(status.as_u16()), &raw),
                    &secrets,
                ))
            } else {
                ModelResponse::completed(raw.clone(), tools_enabled)
            };
            let event_type = if raw.get("error").is_some_and(|e| !e.is_null()) {
                "response.failed"
            } else if raw.get("status").and_then(Value::as_str) == Some("completed") {
                "response.completed"
            } else {
                "response.incomplete"
            };
            let event = json!({"type":event_type,"response":raw});
            if let Some(sender) = &events {
                send_event(sender, event, &cancel)
                    .await
                    .map_err(|error| raw_failure(error, raw.clone()))?;
            }
            return result.map_err(|error| raw_failure(error, raw));
        }
        if content_type != "text/event-stream" {
            return Err(ModelError::new(
                "protocol",
                "Expected text/event-stream or application/json from Responses endpoint",
            )
            .into());
        }
        let mut stream = response.bytes_stream();
        let mut parsed = ResponseStream::default();
        let mut raw_response = None;
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(failure(cancelled(), &parsed, raw_response)),
                next = stream.next() => next,
            };
            let Some(chunk) = next else {
                break;
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => {
                    return Err(failure(
                        ModelError::new("transport", "HTTP stream interrupted"),
                        &parsed,
                        raw_response,
                    ));
                }
            };
            let values = match parsed.push(&chunk) {
                Ok(values) => values,
                Err(error) => return Err(failure(redact(error, &secrets), &parsed, raw_response)),
            };
            for value in values {
                if let Some(raw) = value.get("response") {
                    raw_response = Some(raw.clone());
                }
                if let Some(sender) = &events
                    && let Err(error) = send_event(sender, value, &cancel).await
                {
                    return Err(failure(error, &parsed, raw_response));
                }
            }
            // A terminal SSE event is the protocol boundary. Waiting for a proxy
            // to close its HTTP connection would retain resources indefinitely.
            if parsed.is_terminal() {
                break;
            }
        }
        if cancel.is_cancelled() {
            return Err(failure(cancelled(), &parsed, raw_response));
        }
        let partial_text = parsed.partial_text.clone();
        let completed_items = parsed.completed_items.clone();
        parsed
            .finish(tools_enabled)
            .map_err(|error| AttemptFailure {
                error: redact(error, &secrets),
                partial_text,
                completed_items,
                raw_response,
            })
    }
    pub async fn embeddings(
        &self,
        model: &str,
        input: &[String],
        expected_dimension: Option<usize>,
        cancel: CancellationToken,
    ) -> Result<EmbeddingResponse, ModelError> {
        if model.trim().is_empty() || input.is_empty() || expected_dimension == Some(0) {
            return Err(ModelError::new(
                "input_invalid",
                "Embedding model/input must be nonempty and dimension positive",
            ));
        }
        let request = json!({"model":model,"input":input,"encoding_format":"float"});
        let (response, secrets) = self
            .send(ApiEndpoint::Embeddings, &request, &cancel)
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_error(response, &cancel, &secrets).await);
        }
        let bytes = bounded_body(response, MAX_EMBEDDING_BYTES, &cancel).await?;
        let raw: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ModelError::new("protocol", "Malformed embeddings JSON"))?;
        if raw.get("error").is_some_and(|e| !e.is_null()) {
            return Err(redact(
                ModelError::provider(Some(status.as_u16()), &raw),
                &secrets,
            ));
        }
        let data = raw
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| ModelError::new("protocol", "Embedding response has no data array"))?;
        if data.len() != input.len() {
            return Err(ModelError::new(
                "protocol",
                "Embedding count differs from input count",
            ));
        }
        let mut vectors = vec![None; input.len()];
        let mut dimension = expected_dimension;
        for item in data {
            let index = item
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|i| usize::try_from(i).ok())
                .filter(|i| *i < input.len())
                .ok_or_else(|| ModelError::new("protocol", "Invalid embedding index"))?;
            if vectors[index].is_some() {
                return Err(ModelError::new("protocol", "Duplicate embedding index"));
            }
            let values = item
                .get("embedding")
                .and_then(Value::as_array)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| ModelError::new("protocol", "Empty or non-array embedding"))?;
            if dimension.is_some_and(|d| d != values.len()) {
                return Err(ModelError::new(
                    "dimension_mismatch",
                    "Embedding dimension does not match the active space",
                ));
            }
            dimension = Some(values.len());
            let vector = values
                .iter()
                .map(|value| {
                    value
                        .as_f64()
                        .map(|v| v as f32)
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| {
                            ModelError::new(
                                "protocol",
                                "Embedding contains a non-finite or non-numeric value",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            vectors[index] = Some(vector);
        }
        Ok(EmbeddingResponse {
            vectors: vectors
                .into_iter()
                .map(|v| v.expect("all validated indices present"))
                .collect(),
            dimension: dimension.expect("nonempty validated embeddings"),
            usage: raw.get("usage").cloned(),
        })
    }
}
fn cancelled() -> ModelError {
    ModelError::new("cancelled", "Model request was cancelled")
}
fn raw_failure(error: ModelError, raw: Value) -> AttemptFailure {
    let mut partial_text = String::new();
    let mut completed_items = BTreeMap::new();
    if let Some(output) = raw.get("output").and_then(Value::as_array) {
        for (index, item) in output.iter().enumerate() {
            if item
                .get("status")
                .and_then(Value::as_str)
                .is_none_or(|s| s == "completed")
            {
                completed_items.insert(index as u64, item.clone());
            }
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if let Some(text) = part
                        .get("text")
                        .or_else(|| part.get("refusal"))
                        .and_then(Value::as_str)
                    {
                        partial_text.push_str(text);
                    }
                }
            }
        }
    }
    AttemptFailure {
        error,
        partial_text,
        completed_items,
        raw_response: Some(raw),
    }
}
fn failure(
    error: ModelError,
    parsed: &ResponseStream,
    raw_response: Option<Value>,
) -> AttemptFailure {
    AttemptFailure {
        error,
        partial_text: parsed.partial_text.clone(),
        completed_items: parsed.completed_items.clone(),
        raw_response,
    }
}
async fn send_event(
    sender: &mpsc::Sender<Value>,
    value: Value,
    cancel: &CancellationToken,
) -> Result<(), ModelError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(cancelled()),
        result = sender.send(value) => result.map_err(|_| ModelError::new("consumer_closed", "Model event consumer closed before persistence")),
    }
}
async fn bounded_body(
    response: Response,
    max: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ModelError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    loop {
        let next = tokio::select! { biased; _ = cancel.cancelled() => return Err(cancelled()), next = stream.next() => next };
        let Some(chunk) = next else {
            return Ok(body);
        };
        let chunk = chunk.map_err(|_| ModelError::new("transport", "HTTP body interrupted"))?;
        if chunk.len() > max.saturating_sub(body.len()) {
            return Err(ModelError::new(
                "input_limit",
                "HTTP response body exceeded byte limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
}
async fn http_error(
    response: Response,
    cancel: &CancellationToken,
    secrets: &[String],
) -> ModelError {
    let status = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    let mut truncated = false;
    loop {
        let next = tokio::select! { biased; _ = cancel.cancelled() => return cancelled(), next = stream.next() => next };
        let Some(chunk) = next else {
            break;
        };
        let Ok(chunk) = chunk else {
            break;
        };
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
                .take(16_384)
                .collect(),
            provider_code: None,
            status: Some(status),
        },
    };
    if error.message.is_empty() {
        error.message = "Proxy returned an empty HTTP error body".into();
    }
    if truncated {
        error.message.push_str(" [HTTP error body truncated]");
    }
    redact(error, secrets)
}
fn redact(mut error: ModelError, secrets: &[String]) -> ModelError {
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        error.message = error.message.replace(secret, "[redacted]");
        if let Some(code) = &mut error.provider_code {
            *code = code.replace(secret, "[redacted]");
        }
    }
    if error.message.chars().count() > 16_384 {
        error.message =
            error.message.chars().take(16_350).collect::<String>() + " [diagnostic truncated]";
    }
    error.provider_code = error
        .provider_code
        .map(|code| code.chars().take(256).collect());
    error
}
