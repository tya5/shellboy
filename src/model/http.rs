//! Native asynchronous HTTP transport. No request retry or alternate API fallback.
use super::ModelError;
use crate::config::{ApiEndpoint, Config, Connection};
mod body;
mod clients;
pub(crate) use clients::HttpClients;
mod deadline;
mod response;
use body::read_json;
use deadline::{Deadline, transport_error};
use reqwest::{
    Client, Response,
    header::{HeaderMap, HeaderName, HeaderValue},
};
pub use response::{AttemptFailure, ResponseFacts, ResponseReceived};
#[cfg(test)]
use response::{parse_retry_after, send_event};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAX_EMBEDDING_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODEL_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_MODEL_CATALOG_ENTRIES: usize = 10_000;
const MAX_MODEL_ID_BYTES: usize = 1024;
/// Bounded, ephemeral observations on the driver's existing event channel.
pub(crate) enum ObservedEvent {
    Received(std::time::Instant),
    Model {
        value: Value,
        received_at: std::time::Instant,
    },
}
enum Events {
    Plain(mpsc::Sender<Value>),
    Observed(mpsc::Sender<ObservedEvent>),
}
impl Events {
    fn received(&self, at: std::time::Instant) {
        if let Self::Observed(sender) = self {
            let _ = sender.try_send(ObservedEvent::Received(at));
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
    connection_name: String,
}
impl HttpModelClient {
    pub fn new(connection: Connection) -> Result<Self, ModelError> {
        Self::for_connection("selected", connection)
    }
    pub fn for_connection(name: &str, connection: Connection) -> Result<Self, ModelError> {
        let mut config = Config::default();
        config.connections.insert(name.into(), connection.clone());
        config
            .validate()
            .map_err(|e| ModelError::new("config_invalid", e.to_string()))?;
        let client = Client::builder()
            .tls_danger_accept_invalid_certs(!connection.ssl_verify)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|_| ModelError::new("transport", "Unable to initialize HTTP client"))?;
        Ok(Self {
            client,
            connection,
            connection_name: name.into(),
        })
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
        method: reqwest::Method,
        request: Option<&Value>,
        cancel: &CancellationToken,
        deadline: &Deadline,
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
        let mut outgoing = self
            .client
            .request(method, endpoint)
            .headers(headers)
            .header(reqwest::header::ACCEPT, accept);
        if let Some(body) = request {
            outgoing = outgoing.json(body);
        }
        deadline
            .wait(cancel, "waiting for response headers", outgoing.send())
            .await?
            .map(|response| (response, secrets))
            .map_err(|error| transport_error(error, "before response headers"))
    }
    /// Fetch the configured proxy's catalog without embedding a product-specific model list.
    /// The deadline includes both response headers and the complete bounded body.
    pub async fn models(&self, cancel: CancellationToken) -> Result<Vec<String>, ModelError> {
        let mut deadline =
            Deadline::new(ApiEndpoint::Models, &self.connection, &self.connection_name)?;
        self.models_request(&cancel, &mut deadline).await
    }
    async fn models_request(
        &self,
        cancel: &CancellationToken,
        deadline: &mut Deadline,
    ) -> Result<Vec<String>, ModelError> {
        let (response, secrets) = self
            .send(
                ApiEndpoint::Models,
                reqwest::Method::GET,
                None,
                cancel,
                deadline,
            )
            .await?;
        deadline.received();
        let raw = read_json(
            response,
            MAX_MODEL_CATALOG_BYTES,
            "Proxy returned malformed model list JSON",
            cancel,
            deadline,
            None,
            None,
            &secrets,
        )
        .await?
        .into_result()?;
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
        let mut deadline = Deadline::new(
            ApiEndpoint::Embeddings,
            &self.connection,
            &self.connection_name,
        )?;
        let request = json!({"model":model,"input":input,"encoding_format":"float"});
        let (response, secrets) = self
            .send(
                ApiEndpoint::Embeddings,
                reqwest::Method::POST,
                Some(&request),
                &cancel,
                &deadline,
            )
            .await?;
        deadline.received();
        let raw = read_json(
            response,
            MAX_EMBEDDING_BYTES,
            "Malformed embeddings JSON",
            &cancel,
            &mut deadline,
            None,
            None,
            &secrets,
        )
        .await?
        .into_result()?;
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

#[cfg(test)]
mod observation_tests {
    use super::*;
    #[test]
    fn retry_after_accepts_seconds_and_http_dates_but_not_arbitrary_headers() {
        use std::time::Duration;
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after("999999999999999999999999999"),
            Some(Duration::from_secs(u64::MAX))
        );
        assert!(
            parse_retry_after("Fri, 01 Jan 2100 00:00:00 GMT").unwrap() > Duration::from_secs(600)
        );
        assert_eq!(parse_retry_after("600"), Some(Duration::from_secs(600)));
        assert_eq!(parse_retry_after("601"), Some(Duration::from_secs(601)));
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(Duration::ZERO)
        );
        for invalid in ["", "-1", "1.5", "retry soon", "1, 2"] {
            assert_eq!(parse_retry_after(invalid), None, "{invalid}");
        }
    }
    #[tokio::test]
    async fn model_event_preserves_receive_time_when_heartbeat_notification_is_full() {
        let (sender, mut receiver) = mpsc::channel(1);
        let events = Events::Observed(sender);
        let before = std::time::Instant::now();
        events.received(before);
        let after = before + std::time::Duration::from_secs(1);
        events.received(after);
        let model = json!({"type":"response.output_text.delta","delta":"ok"});
        let cancel = CancellationToken::new();
        let send = send_event(&events, model.clone(), &cancel, after);
        let receive = async {
            assert!(
                matches!(receiver.recv().await, Some(ObservedEvent::Received(at)) if at == before)
            );
            let Some(ObservedEvent::Model { value, received_at }) = receiver.recv().await else {
                panic!("model event lost")
            };
            assert_eq!(value, model);
            assert_eq!(received_at, after);
        };
        let (result, ()) = tokio::join!(send, receive);
        result.unwrap();
    }

    #[tokio::test]
    async fn error_body_bytes_refresh_transport_observation_before_timeout() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            sync::oneshot,
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = HttpModelClient::for_connection("error_body_fixture", Connection {
                base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
                stream_idle_timeout_ms: 1000,
                ..Default::default()
            }).unwrap();
            let (continue_body, wait_for_body) = oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let mut bytes = [0; 1024];
                    let count = stream.read(&mut bytes).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&bytes[..count]);
                }
                stream.write_all(b"HTTP/1.1 503 Fixture\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":").await.unwrap();
                wait_for_body.await.unwrap();
                stream.write_all(b"{").await.unwrap();
                std::future::pending::<()>().await;
            });
            let (send, mut receive) = mpsc::channel(4);
            let response = tokio::spawn(async move {
                client.response_observed(json!({"model":"fixture"}), CancellationToken::new(), Some(send)).await
            });
            // Headers and the first partial error-body bytes are distinct
            // transport facts, even though neither is an assistant delta.
            for _ in 0..2 {
                assert!(matches!(receive.recv().await, Some(ObservedEvent::Received(_))));
            }
            continue_body.send(()).unwrap();
            assert!(matches!(receive.recv().await, Some(ObservedEvent::Received(_))));
            let failure = response.await.unwrap().unwrap_err();
            assert_eq!(failure.error.kind, "timeout");
            assert_eq!(failure.error.status, Some(503));
            assert!(failure.error.message.contains("receiving HTTP error body"));
            assert!(failure.partial_text.is_empty());
            server.abort();
            let _ = server.await;
        }).await.expect("error body receive observations or deadline stalled");
    }
}
