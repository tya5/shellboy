//! Native asynchronous HTTP transport. No request retry or alternate API fallback.
use super::ModelError;
use crate::config::{ApiEndpoint, Config, Connection};
mod body;
mod clients;
pub(crate) use clients::HttpClients;
mod deadline;
mod observation;
pub use observation::{ConnectionReuse, HttpObservation, HttpPhase};
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
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicUsize, Ordering},
};
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

/// Borrowed state for one HTTP request; no identity or secrets outlive it.
struct RequestContext<'a> {
    cancel: &'a CancellationToken,
    deadline: &'a mut Deadline,
    events: Option<&'a Events>,
    secrets: &'a [String],
}

#[derive(Debug)]
pub struct EmbeddingResponse {
    /// Vectors in the same order as input, independent of provider data order.
    pub vectors: Vec<Vec<f32>>,
    pub dimension: usize,
    pub usage: Option<Value>,
}
struct ClientPool {
    client: Result<Client, ModelError>,
    generation: u64,
    issued: u64,
}
#[derive(Clone)]
pub struct HttpModelClient {
    pool: Arc<RwLock<ClientPool>>,
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
        let client = Self::build_client(&connection)?;
        Ok(Self {
            pool: Arc::new(RwLock::new(ClientPool {
                client: Ok(client),
                generation: 0,
                issued: 0,
            })),
            connection,
            connection_name: name.into(),
        })
    }
    fn build_client(connection: &Connection) -> Result<Client, ModelError> {
        Client::builder()
            .tls_danger_accept_invalid_certs(!connection.ssl_verify)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|_| ModelError::new("transport", "Unable to initialize HTTP client"))
    }
    fn snapshot_pool(&self) -> Result<(Client, u64, ConnectionReuse), ModelError> {
        let mut pool = self.pool.write().expect("HTTP pool lock poisoned");
        let client = pool.client.clone()?;
        let reuse = if pool.issued == 0 {
            ConnectionReuse::FreshPool
        } else {
            ConnectionReuse::Unknown
        };
        pool.issued = pool.issued.saturating_add(1);
        Ok((client, pool.generation, reuse))
    }
    fn retire_pool(&self, generation: u64) {
        if self
            .pool
            .read()
            .expect("HTTP pool lock poisoned")
            .generation
            != generation
        {
            return;
        }
        // Build outside the lock. Existing in-flight requests retain their
        // original pool, while subsequent attempts use the replacement.
        let replacement = Self::build_client(&self.connection);
        let mut pool = self.pool.write().expect("HTTP pool lock poisoned");
        if pool.generation == generation {
            pool.client = replacement;
            pool.generation = pool.generation.saturating_add(1);
            pool.issued = 0;
        }
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
        session_header: Option<&HeaderValue>,
    ) -> Result<(Response, Vec<String>, HttpObservation), ModelError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let (mut headers, mut secrets) = self.headers()?;
        if matches!(endpoint, ApiEndpoint::Responses)
            && let Some(value) = session_header
        {
            headers.insert("x-shellboy-session-id", value.clone());
            if let Ok(value) = value.to_str() {
                secrets.push(value.to_owned());
            }
        }
        let endpoint_kind = endpoint;
        let endpoint = self
            .connection
            .endpoint(endpoint)
            .map_err(|e| ModelError::new("config_invalid", e.to_string()))?;
        let accept = match endpoint_kind {
            ApiEndpoint::Responses => "text/event-stream, application/json",
            ApiEndpoint::Embeddings | ApiEndpoint::Models => "application/json",
        };
        let (client, generation, reuse) = self.snapshot_pool()?;
        let mut outgoing = client
            .request(method, endpoint)
            .headers(headers)
            .header(reqwest::header::ACCEPT, accept);
        let consumed = Arc::new(AtomicUsize::new(0));
        let mut body_len = 0;
        if let Some(body) = request {
            let bytes = serde_json::to_vec(body)
                .map_err(|_| ModelError::new("input_invalid", "Cannot encode HTTP request"))?;
            body_len = bytes.len();
            let progress = consumed.clone();
            let stream = futures_util::stream::unfold((bytes, 0), move |(bytes, offset)| {
                let progress = progress.clone();
                async move {
                    if offset == bytes.len() {
                        return None;
                    }
                    let end = (offset + 16 * 1024).min(bytes.len());
                    let chunk = bytes[offset..end].to_vec();
                    progress.store(end, Ordering::Relaxed);
                    Some((Ok::<_, std::io::Error>(chunk), (bytes, end)))
                }
            });
            outgoing = outgoing
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::CONTENT_LENGTH, body_len)
                .body(reqwest::Body::wrap_stream(stream));
        }
        let started = std::time::Instant::now();
        let outcome = deadline
            .wait_headers(cancel, outgoing.send())
            .await
            .and_then(|result| {
                result.map_err(|error| transport_error(error, "before response headers"))
            });
        let consumed_bytes = consumed.load(Ordering::Relaxed);
        let mut observation = HttpObservation {
            phase: if consumed_bytes < body_len {
                HttpPhase::UploadSourceIncomplete
            } else {
                HttpPhase::AwaitingHeadersDeliveryUnknown
            },
            request_body_bytes: body_len,
            body_source_bytes_consumed: consumed_bytes,
            header_elapsed_ms: None,
            pool_generation: generation,
            connection_reuse: reuse,
        };
        match outcome {
            Ok(response) => {
                observation.phase = HttpPhase::BodyReceive;
                observation.header_elapsed_ms =
                    Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                Ok((response, secrets, observation))
            }
            Err(mut error) => {
                error.message.push_str("; ");
                error.message.push_str(observation.hint());
                error.http_observation = Some(Box::new(observation));
                self.retire_pool(generation);
                Err(error)
            }
        }
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
        let (response, secrets, _observation) = self
            .send(
                ApiEndpoint::Models,
                reqwest::Method::GET,
                None,
                cancel,
                deadline,
                None,
            )
            .await?;
        deadline.received();
        let raw = read_json(
            response,
            MAX_MODEL_CATALOG_BYTES,
            "Proxy returned malformed model list JSON",
            &mut RequestContext {
                cancel,
                deadline,
                events: None,
                secrets: &secrets,
            },
            None,
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
        let (response, secrets, _observation) = self
            .send(
                ApiEndpoint::Embeddings,
                reqwest::Method::POST,
                Some(&request),
                &cancel,
                &deadline,
                None,
            )
            .await?;
        deadline.received();
        let raw = read_json(
            response,
            MAX_EMBEDDING_BYTES,
            "Malformed embeddings JSON",
            &mut RequestContext {
                cancel: &cancel,
                deadline: &mut deadline,
                events: None,
                secrets: &secrets,
            },
            None,
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
                client.response_observed("fixture-thread", json!({"model":"fixture"}), CancellationToken::new(), Some(send)).await
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

#[cfg(test)]
mod pool_generation_tests {
    use super::*;
    #[test]
    fn late_old_failure_cannot_retire_new_pool_or_fabricate_reuse() {
        let client = HttpModelClient::new(Connection {
            base_url: "http://127.0.0.1:1/v1".into(),
            ..Default::default()
        })
        .unwrap();
        let (_, old, first) = client.snapshot_pool().unwrap();
        assert_eq!(old, 0);
        assert_eq!(first, ConnectionReuse::FreshPool);
        let (_, same, other) = client.snapshot_pool().unwrap();
        assert_eq!(same, old);
        assert_eq!(other, ConnectionReuse::Unknown);
        client.retire_pool(old);
        let (_, new, fresh) = client.snapshot_pool().unwrap();
        assert_eq!(new, 1);
        assert_eq!(fresh, ConnectionReuse::FreshPool);
        client.retire_pool(old);
        let (_, still_new, unknown) = client.snapshot_pool().unwrap();
        assert_eq!(still_new, new);
        assert_eq!(unknown, ConnectionReuse::Unknown);
    }
}
