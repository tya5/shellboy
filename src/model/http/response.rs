//! Responses reception preserves bounded recovery data and reports facts only.
use super::body::{http_error_observed, read_json, redact};
use super::*;
use crate::config::Api;
use crate::model::{MAX_RESPONSE_BYTES, ModelResponse, ResponseStream, dialect};
use futures_util::StreamExt;
use std::collections::BTreeMap;
const TERMINAL_DRAIN_BYTES: usize = 64 * 1024;
const TERMINAL_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResponseReceived {
    #[default]
    None,
    Metadata,
    Output,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct ResponseFacts {
    pub received: ResponseReceived,
    pub status: Option<u16>,
    pub retry_after: Option<std::time::Duration>,
    pub retry_after_present: bool,
    /// A terminal SSE event or a fully parsed JSON response document arrived.
    /// Validation failure after that boundary is distinct from interrupted transport.
    pub terminal_received: bool,
    /// A bounded non-success HTTP error body reached EOF.
    pub http_error_complete: bool,
    /// Actual time spent waiting for connection admission, if measured.
    pub admission_wait_ms: Option<u64>,
    /// Transport observations do not establish peer receipt or processing.
    pub http_observation: Option<HttpObservation>,
}
impl ResponseFacts {
    pub(crate) fn rate_metadata(self) -> Value {
        json!({"status": self.status, "retry_after_present": self.status.map(|_| self.retry_after_present),
            "retry_after_ms": self.retry_after.map(|d| d.as_millis().min(u64::MAX as u128) as u64),
            "admission_wait_ms": self.admission_wait_ms})
    }
    fn observe_stream(&mut self, parsed: &ResponseStream) {
        if parsed.has_output() {
            self.received = ResponseReceived::Output;
        }
        self.terminal_received = parsed.is_terminal();
    }
}
#[derive(Debug)]
pub struct AttemptFailure {
    pub error: ModelError,
    pub partial_text: String,
    pub partial_reasoning: String,
    pub completed_items: BTreeMap<u64, Value>,
    pub raw_response: Option<Value>,
    pub facts: ResponseFacts,
}
impl From<ModelError> for AttemptFailure {
    fn from(error: ModelError) -> Self {
        Self {
            error,
            partial_text: String::new(),
            partial_reasoning: String::new(),
            completed_items: BTreeMap::new(),
            raw_response: None,
            facts: ResponseFacts::default(),
        }
    }
}
impl HttpModelClient {
    // Recovery needs the bounded partial text, items, and original response together.
    #[allow(clippy::result_large_err)]
    pub async fn response(
        &self,
        request: Value,
        cancel: CancellationToken,
        events: Option<mpsc::Sender<Value>>,
    ) -> Result<ModelResponse, AttemptFailure> {
        let admission = self.public_rate_admission(&cancel).await?;
        self.response_with_admission(request, cancel, events.map(Events::Plain), None, admission)
            .await
    }
    /// Attach the continuing Thread identity only to this Responses request.
    /// A shared client never owns the identity of a particular conversation.
    #[allow(clippy::result_large_err)]
    pub async fn response_for_thread(
        &self,
        thread: &str,
        request: Value,
        cancel: CancellationToken,
        events: Option<mpsc::Sender<Value>>,
    ) -> Result<ModelResponse, AttemptFailure> {
        let admission = self.public_rate_admission(&cancel).await?;
        self.response_with_admission(
            request,
            cancel,
            events.map(Events::Plain),
            Some(thread),
            admission,
        )
        .await
    }

    #[allow(clippy::result_large_err)]
    async fn public_rate_admission(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(RateAdmission, u64), AttemptFailure> {
        if cancel.is_cancelled() {
            return Err(cancelled().into());
        }
        match self.reserve_rate_admission(None)? {
            RateReservation::Ready(admission) => Ok((admission, 0)),
            RateReservation::Waiting(waiter) => {
                let started = tokio::time::Instant::now();
                let lease = waiter.wait(cancel).await?;
                Ok((
                    RateAdmission::Lease(lease),
                    started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                ))
            }
        }
    }
    #[allow(clippy::result_large_err)]
    async fn response_with_admission(
        &self,
        request: Value,
        cancel: CancellationToken,
        events: Option<Events>,
        thread: Option<&str>,
        (admission, elapsed): (RateAdmission, u64),
    ) -> Result<ModelResponse, AttemptFailure> {
        let mut result = match &admission {
            RateAdmission::QuotaBlocked(fact) => Err(fact.error().into()),
            RateAdmission::Lease(_) => {
                self.response_inner(request, cancel, events, thread, None)
                    .await
            }
        };
        match &mut result {
            Ok(response) => response.response_facts.admission_wait_ms = Some(elapsed),
            Err(failure) => failure.facts.admission_wait_ms = Some(elapsed),
        }
        if let RateAdmission::Lease(lease) = &admission {
            let facts = match &result {
                Ok(response) => Some(response.response_facts),
                Err(failure)
                    if failure.facts.terminal_received || failure.facts.http_error_complete =>
                {
                    Some(failure.facts)
                }
                Err(_) => None,
            };
            if let Some(status) = facts.and_then(|facts| facts.status) {
                lease.complete(status);
            }
        }
        result
    }

    #[allow(clippy::result_large_err)]
    pub(crate) async fn response_for_thread_pre_admitted(
        &self,
        thread: &str,
        request: Value,
        cancel: CancellationToken,
    ) -> Result<ModelResponse, AttemptFailure> {
        self.response_inner(request, cancel, None, Some(thread), None)
            .await
    }
    // Preserve the same bounded recovery payload as response().
    #[allow(clippy::result_large_err)]
    pub(crate) async fn response_observed(
        &self,
        thread: &str,
        request: Value,
        cancel: CancellationToken,
        events: Option<mpsc::Sender<ObservedEvent>>,
        bypass_usage_limit: Option<crate::model::usage_limit::UsageLimit>,
    ) -> Result<ModelResponse, AttemptFailure> {
        self.response_inner(
            request,
            cancel,
            events.map(Events::Observed),
            Some(thread),
            bypass_usage_limit,
        )
        .await
    }
    #[allow(clippy::result_large_err)]
    async fn response_inner(
        &self,
        request: Value,
        cancel: CancellationToken,
        events: Option<Events>,
        thread: Option<&str>,
        bypass_usage_limit: Option<crate::model::usage_limit::UsageLimit>,
    ) -> Result<ModelResponse, AttemptFailure> {
        if cancel.is_cancelled() {
            return Err(cancelled().into());
        }
        // Only a driver-authorized, budgeted retry may bypass the shared block.
        if let Some(active) = self.active_usage_limit() {
            let is_scheduled_retry = bypass_usage_limit.is_some_and(|expected| {
                active.kind == expected.kind
                    && active.resets_at == expected.resets_at
                    && active.limit_window_minutes == expected.limit_window_minutes
            });
            if !is_scheduled_retry {
                return Err(active.error().into());
            }
        }
        let mut deadline = Deadline::new(
            ApiEndpoint::Responses,
            &self.connection,
            &self.connection_name,
        )?;
        let mut facts = ResponseFacts::default();
        let tools_enabled = request
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty());
        let (request, session_header) = self.wire_request(request, thread)?;
        let result = self
            .response_attempt(
                request,
                tools_enabled,
                RequestContext {
                    cancel: &cancel,
                    deadline: &mut deadline,
                    events: events.as_ref(),
                    secrets: &[],
                },
                &mut facts,
                session_header,
            )
            .await;
        if result.is_ok()
            && let Some(expected) = bypass_usage_limit
        {
            self.clear_usage_limit_if(expected);
        }
        result
            .map(|mut response| {
                response.response_facts = facts;
                response
            })
            .map_err(|mut failure| {
                if let Some(fact) = failure.error.usage_limit.as_deref().copied() {
                    self.remember_usage_limit(fact);
                }
                if let Some(raw) = &mut failure.raw_response {
                    crate::usage::normalize_response(raw);
                    crate::model::cache_diagnostics::normalize_response(raw);
                }
                if let Some(observation) = facts
                    .http_observation
                    .or_else(|| failure.error.http_observation.as_deref().copied())
                {
                    if failure.error.http_observation.is_none() {
                        // A complete provider/protocol failure is not stalled
                        // reception. Keep its original message while attaching
                        // transport facts independently.
                        if matches!(failure.error.kind.as_str(), "timeout" | "transport")
                            || (failure.error.kind == "incomplete" && !facts.terminal_received)
                        {
                            failure.error.message.push_str("; ");
                            failure.error.message.push_str(observation.hint());
                        }
                        failure.error.http_observation = Some(Box::new(observation));
                    }
                    facts.http_observation = Some(observation);
                    if Self::retires_response_pool(&failure.error, facts.terminal_received) {
                        self.retire_pool(observation.pool_generation);
                    }
                }
                failure.facts = facts;
                failure
            })
    }
    fn prepare_session(
        &self,
        thread: Option<&str>,
        request: &mut Value,
    ) -> Result<Option<HeaderValue>, ModelError> {
        let Some(thread) =
            thread.filter(|_| self.connection.session_header || self.connection.prompt_cache_key)
        else {
            return Ok(None);
        };
        // Real Thread IDs are UUIDs. Accept short internal fixture IDs too, but
        // never put control characters or an oversized identifier on the wire.
        if thread.is_empty()
            || thread.len() > 64
            || !thread
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(ModelError::new(
                "input_invalid",
                "Thread session ID must contain 1 to 64 ASCII letters, digits, hyphens or underscores",
            ));
        }
        if self.connection.prompt_cache_key {
            request
                .as_object_mut()
                .ok_or_else(|| ModelError::new("protocol", "Responses request must be an object"))?
                .insert("prompt_cache_key".into(), json!(thread));
        }
        if self.connection.session_header {
            let mut value = HeaderValue::from_str(thread)
                .map_err(|_| ModelError::new("input_invalid", "Invalid Thread session header"))?;
            value.set_sensitive(true);
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }
    pub(crate) fn cache_diagnostics_enabled(&self) -> bool {
        self.connection.api == crate::config::Api::Responses
            && self.connection.prompt_cache_diagnostics
    }
    /// The exact conversation request body this client sends for a canonical
    /// request, and the session header value. Pure: no I/O, no credentials.
    /// `shellboy thread request` uses the same function, so an inspected body
    /// is byte-identical to the one `send` serializes.
    pub(crate) fn wire_request(
        &self,
        request: Value,
        thread: Option<&str>,
    ) -> Result<(Value, Option<HeaderValue>), ModelError> {
        self.wire_request_tagged(request, thread, None)
            .map(|(wire, header, _)| (wire, header))
    }
    /// Measured UTF-8 bytes of the body [`Self::wire_request`] builds for this
    /// request, partitioned by the source of each input item (#186). `tags`
    /// has one entry per canonical `input` item. `None` when the tags do not
    /// match the input; the body itself is never changed by accounting.
    pub(crate) fn input_bytes(
        &self,
        request: Value,
        tags: &[crate::model::wire_bytes::ItemTag],
        thread: Option<&str>,
    ) -> Result<Option<crate::model::wire_bytes::InputBytes>, ModelError> {
        if request["input"].as_array().map(Vec::len) != Some(tags.len()) {
            return Ok(None);
        }
        let (wire, _, labels) = self.wire_request_tagged(request, thread, Some(tags))?;
        Ok(crate::model::wire_bytes::measure(&wire, &labels))
    }
    #[allow(clippy::type_complexity)]
    fn wire_request_tagged(
        &self,
        mut request: Value,
        thread: Option<&str>,
        tags: Option<&[crate::model::wire_bytes::ItemTag]>,
    ) -> Result<(Value, Option<HeaderValue>, crate::model::wire_bytes::Labels), ModelError> {
        let session_header = self.prepare_session(thread, &mut request)?;
        let Some(object) = request.as_object_mut() else {
            return Err(ModelError::new(
                "protocol",
                "Responses request must be an object",
            ));
        };
        object.insert("stream".into(), Value::Bool(true));
        object.insert("store".into(), Value::Bool(false));
        if !self.cache_diagnostics_enabled() {
            object.remove("prompt_cache_options");
        }
        // The hint reads Shellboy's own roles, before context_role maps them.
        let cache = self
            .connection
            .prompt_caching
            .then(|| dialect::CacheHint::of(&request));
        crate::model::apply_context_role(&mut request, self.connection.context_role);
        let (mut request, labels) = dialect::encode_tagged(
            self.connection.api,
            request,
            self.connection
                .max_tokens
                .unwrap_or(crate::config::DEFAULT_MESSAGES_MAX_TOKENS),
            cache,
            tags,
        )?;
        // Shellboy never interprets these. Config validation rejects any key
        // Shellboy owns, so a plain insert can neither override nor reorder.
        if let Some(object) = request.as_object_mut() {
            for (key, value) in &self.connection.extra_body {
                object.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        Ok((request, session_header, labels))
    }
    // Keep the existing bounded recovery payload intact across body parsing.
    #[allow(clippy::result_large_err)]
    async fn response_attempt(
        &self,
        request: Value,
        tools_enabled: bool,
        context: RequestContext<'_>,
        facts: &mut ResponseFacts,
        session_header: Option<HeaderValue>,
    ) -> Result<ModelResponse, AttemptFailure> {
        let (response, secrets, observation) = self
            .send(
                ApiEndpoint::Responses,
                reqwest::Method::POST,
                Some(&request),
                context.cancel,
                context.deadline,
                session_header.as_ref(),
            )
            .await?;
        facts.http_observation = Some(observation);
        let mut context = RequestContext {
            cancel: context.cancel,
            deadline: context.deadline,
            events: context.events,
            secrets: &secrets,
        };
        context.deadline.received();
        if let Some(events) = context.events {
            events.received(context.deadline.received_at());
        }
        let status = response.status();
        facts.status = Some(status.as_u16());
        facts.received = ResponseReceived::Metadata;
        let retry_after = response.headers().get(reqwest::header::RETRY_AFTER);
        facts.retry_after_present = retry_after.is_some();
        facts.retry_after = retry_after
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after);
        if status.as_u16() == 429
            && let Some(diagnostics) = self.diagnostics.get()
        {
            diagnostics.record_rate_limit(&self.connection_name, *facts);
        }
        if !status.is_success() {
            let limit = (status.as_u16() == 429)
                .then(|| {
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| {
                            crate::model::usage_limit::UsageLimit::retry_after_header(
                                v,
                                chrono::Utc::now(),
                            )
                        })
                })
                .flatten();
            if let Some(fact) = limit {
                // The header already establishes the cooldown; do not wait for
                // an error body that may be stalled or arbitrarily large.
                let mut error = fact.error();
                error.status = Some(429);
                return Err(error.into());
            }
            return Err(http_error_observed(
                response,
                &mut context,
                &mut facts.http_error_complete,
            )
            .await
            .into());
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
        let outcome = match content_type.as_str() {
            "application/json" => {
                response_json(
                    response,
                    &mut context,
                    facts,
                    tools_enabled,
                    self.connection.api,
                )
                .await
            }
            "text/event-stream" => {
                response_sse(
                    response,
                    &mut context,
                    facts,
                    tools_enabled,
                    self.connection.api,
                )
                .await
            }
            _ => Err(ModelError::new(
                "protocol",
                "Expected text/event-stream or application/json from conversation endpoint",
            )
            .into()),
        };
        outcome.map(|mut response| {
            response.http_observation = Some(observation);
            response
        })
    }
}
#[allow(clippy::result_large_err)]
async fn response_json(
    response: Response,
    context: &mut RequestContext<'_>,
    facts: &mut ResponseFacts,
    tools_enabled: bool,
    api: Api,
) -> Result<ModelResponse, AttemptFailure> {
    let body = read_json(
        response,
        MAX_RESPONSE_BYTES,
        "Proxy returned malformed JSON",
        context,
        Some(&mut *facts),
    )
    .await?;
    facts.terminal_received = true;
    let raw = if api == Api::Responses {
        body.raw
    } else {
        let original = body.raw;
        dialect::decode_json(api, original.clone()).map_err(|error| raw_failure(error, original))?
    };
    if raw
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| !output.is_empty())
    {
        facts.received = ResponseReceived::Output;
    }
    let result = match body.provider_error {
        Some(error) => Err(error),
        None => ModelResponse::completed(raw.clone(), tools_enabled),
    };
    let event_type = if raw.get("error").is_some_and(|e| !e.is_null()) {
        "response.failed"
    } else if raw.get("status").and_then(Value::as_str) == Some("completed") {
        "response.completed"
    } else {
        "response.incomplete"
    };
    let event = json!({"type":event_type,"response":raw});
    if let Some(sender) = context.events {
        send_event(
            sender,
            event,
            context.cancel,
            context.deadline.received_at(),
        )
        .await
        .map_err(|error| raw_failure(error, raw.clone()))?;
    }
    result.map_err(|error| raw_failure(error, raw))
}
#[allow(clippy::result_large_err)]
async fn response_sse(
    response: Response,
    context: &mut RequestContext<'_>,
    facts: &mut ResponseFacts,
    tools_enabled: bool,
    api: Api,
) -> Result<ModelResponse, AttemptFailure> {
    let mut stream = response.bytes_stream();
    let mut parsed = dialect::Stream::new(api);
    let mut raw_response = None;
    loop {
        let next = match context
            .deadline
            .wait(context.cancel, "receiving response body", stream.next())
            .await
        {
            Ok(next) => next,
            Err(error) => return Err(failure(error, &parsed, raw_response)),
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return Err(failure(
                    transport_error(error, "receiving response body"),
                    &parsed,
                    raw_response,
                ));
            }
        };
        if !chunk.is_empty() {
            context.deadline.received();
            if let Some(events) = context.events {
                events.received(context.deadline.received_at());
            }
        }
        let pushed = parsed.push(&chunk);
        facts.observe_stream(&parsed);
        let values = match pushed {
            Ok(values) => values,
            Err(error) => {
                return Err(failure(
                    redact(error, context.secrets),
                    &parsed,
                    raw_response,
                ));
            }
        };
        for value in values {
            if let Some(raw) = value.get("response") {
                raw_response = Some(raw.clone());
            }
            if let Some(sender) = context.events
                && let Err(error) = send_event(
                    sender,
                    value,
                    context.cancel,
                    context.deadline.received_at(),
                )
                .await
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
    if context.cancel.is_cancelled() {
        return Err(failure(cancelled(), &parsed, raw_response));
    }
    let raw_response = parsed.failed_response().cloned().or(raw_response);
    let partial_text = parsed.partial_text.clone();
    let partial_reasoning = parsed.partial_reasoning.clone();
    let completed_items = parsed.completed_items.clone();
    let response = parsed
        .finish(tools_enabled)
        .map_err(|error| AttemptFailure {
            error: redact(error, context.secrets),
            partial_text: partial_text.clone(),
            partial_reasoning: partial_reasoning.clone(),
            completed_items: completed_items.clone(),
            raw_response,
            facts: ResponseFacts::default(),
        })?;
    // Validation already established the response boundary. Only resource
    // cleanup remains: trailing bytes are opaque and cannot change success.
    // Bound both time and bytes rather than waiting for a proxy to close.
    let drain = async {
        let mut received = 0usize;
        while let Some(Ok(chunk)) = stream.next().await {
            received = received.saturating_add(chunk.len());
            if received >= TERMINAL_DRAIN_BYTES {
                break;
            }
        }
    };
    tokio::select! {
        biased;
        _ = context.cancel.cancelled() => return Err(AttemptFailure {
            error: cancelled(), partial_text, partial_reasoning, completed_items,
            raw_response: Some(response.raw), facts: ResponseFacts::default(),
        }),
        _ = tokio::time::timeout(TERMINAL_DRAIN_TIMEOUT, drain) => {},
    }
    Ok(response)
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
        partial_reasoning: String::new(),
        completed_items,
        raw_response: Some(raw),
        facts: ResponseFacts::default(),
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
        partial_reasoning: parsed.partial_reasoning.clone(),
        completed_items: parsed.completed_items.clone(),
        raw_response: parsed.failed_response().cloned().or(raw_response),
        facts: ResponseFacts::default(),
    }
}
pub(super) async fn send_event(
    sender: &Events,
    value: Value,
    cancel: &CancellationToken,
    received_at: std::time::Instant,
) -> Result<(), ModelError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(cancelled()),
        result = async {
            match sender {
                Events::Plain(sender) => sender.send(value).await.map_err(|_| ()),
                Events::Observed(sender) => sender.send(ObservedEvent::Model { value, received_at }).await.map_err(|_| ()),
            }
        } => result.map_err(|_| ModelError::new("consumer_closed", "Model event consumer closed before persistence")),
    }
}
pub(super) fn parse_retry_after(value: &str) -> Option<std::time::Duration> {
    use std::time::Duration;
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        // Preserve long waits so the driver can decline rather than retry early.
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| {
            date.signed_duration_since(chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
        })
}
