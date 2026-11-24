//! Shared Responses attempt persistence and transport handoff.
use super::{
    ModelError, ModelResponse,
    http::{AttemptFailure, RateAdmission, ResponseFacts, ResponseReceived},
};
#[cfg(test)]
use crate::control::state::AttemptTransition;
use crate::protocol::{ApiError, ErrorCode};
use crate::{
    storage::{DbWorker, StorageError},
    usage::StandardUsage,
};
use rusqlite::{Connection as SqlConnection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SummaryGroup {
    pub id: String,
    pub started_at: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct AdmittedOutputLimit {
    #[serde(flatten)]
    resolved: super::http::ResolvedOutputLimit,
    connection: String,
}

impl AdmittedOutputLimit {
    pub(crate) fn stopped(self) -> crate::protocol::AttemptOutputLimit {
        crate::protocol::AttemptOutputLimit {
            stop: true,
            sent_max_tokens: self.resolved.max_tokens,
            source: self.resolved.source,
            connection: self.connection,
        }
    }
}

pub(crate) type RequestOutcome = Result<ModelResponse, AttemptFailure>;

#[allow(clippy::result_large_err)] // Preserve the transport outcome without re-boxing its failure.
async fn send_attempt(
    cancel: &CancellationToken,
    admission: RateAdmission,
    wait_ms: u64,
    send: impl Future<Output = RequestOutcome>,
) -> RequestOutcome {
    let mut outcome = match &admission {
        _ if cancel.is_cancelled() => {
            Err(ModelError::new("cancelled", "Model request was cancelled").into())
        }
        RateAdmission::QuotaBlocked(fact) => Err(fact.error().into()),
        RateAdmission::Lease(_) => send.await,
    };
    let succeeded = outcome.is_ok();
    let facts = match &mut outcome {
        Ok(response) => &mut response.response_facts,
        Err(failure) => &mut failure.facts,
    };
    facts.admission_wait_ms = Some(wait_ms);
    if let RateAdmission::Lease(lease) = admission
        && let Some(status) = facts.status
        && (succeeded || facts.terminal_received || facts.http_error_complete)
    {
        lease.complete(status);
    }
    outcome
}

pub(crate) fn interrupted_failure_body(attempt: &str, failure: &AttemptFailure) -> Value {
    let mut body = failure_body(attempt, failure);
    if let Some(object) = body.as_object_mut() {
        object.remove("error");
    }
    body["reason"] = json!("human_turn_interrupted");
    body
}

fn failure_body(attempt: &str, failure: &AttemptFailure) -> Value {
    let mut error = failure.error.clone();
    error.http_observation = None;
    json!({"attempt_id":attempt,"error":error,"partial_text":failure.partial_text,"partial_reasoning":failure.partial_reasoning,"completed_items":failure.completed_items,"raw_response":failure.raw_response})
}

#[cfg(test)]
pub(crate) async fn persist_received_attempt(
    database: &DbWorker,
    attempt: &str,
    _: AttemptKind,
    cancelled: bool,
    outcome: &RequestOutcome,
) -> Result<StandardUsage, StorageError> {
    let (facts, state, facts_json) = match outcome {
        Ok(response) => (
            response.response_facts,
            "completed",
            json!({"raw_response":response.raw}),
        ),
        Err(failure) => (failure.facts, "failed", failure_body(attempt, failure)),
    };
    persist_outcome(
        database,
        attempt,
        if cancelled { "cancelled" } else { state },
        facts_json,
        facts,
        0,
        &CancellationToken::new(),
    )
    .await
}

async fn persist_outcome(
    database: &DbWorker,
    attempt: &str,
    state: &'static str,
    body: Value,
    facts: ResponseFacts,
    retry_wait_ms: u64,
    cancel: &CancellationToken,
) -> Result<StandardUsage, StorageError> {
    let mut rate = facts.rate_metadata();
    rate["applied_wait_ms"] = json!(
        facts
            .admission_wait_ms
            .unwrap_or(0)
            .saturating_add(retry_wait_ms)
            .min(i64::MAX as u64)
    );
    let http = facts.http_observation.map(|fact| json!(fact));
    let id = attempt.to_owned();
    let cancel = cancel.clone();
    database
        .call(move |db| {
            write_outcome(
                db,
                &id,
                &body,
                if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    state
                },
                &crate::control::state::now(),
                Some(rate),
                http,
            )
        })
        .await
}

pub(crate) fn usage_of(raw: Option<&serde_json::Value>) -> Option<StandardUsage> {
    raw.and_then(|response| response.get("usage"))
        .filter(|usage| usage.is_object())
        .map(StandardUsage::from_value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptKind {
    Conversation,
    Summary,
}

pub(crate) async fn wait_backoff(cancel: &CancellationToken, delay: Duration) -> u64 {
    let start = tokio::time::Instant::now();
    tokio::select! { biased; _ = cancel.cancelled() => {}, _ = tokio::time::sleep(delay) => {} }
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

use super::retry_class::{MAX_RETRY_AFTER, RetryPolicy, classify_retry};
pub(crate) fn classify_attempt_retry(failure: &AttemptFailure, kind: AttemptKind) -> RetryPolicy {
    let AttemptFailure { error, facts, .. } = failure;
    if kind == AttemptKind::Summary && (error.is_context_limit() || error.usage_limit.is_some()) {
        return RetryPolicy::Ineligible;
    }
    if kind == AttemptKind::Conversation
        && facts.terminal_received
        && matches!(facts.status, Some(200..=299))
        && error.kind == "provider"
        && !error.is_context_limit()
        && !error.is_thinking_binding()
        && error.usage_limit.is_none()
        && matches!(
            error.untyped_stream_error_witness(),
            Some(super::StreamProviderStatus {
                value: 429 | 500..=599,
                source: super::StreamProviderStatusSource::StreamErrorCode,
            })
        )
    {
        if let Some(delay) = facts.retry_after
            && delay > MAX_RETRY_AFTER
        {
            return RetryPolicy::Deferred(delay);
        }
        return RetryPolicy::Retry {
            partial_output: facts.received == ResponseReceived::Output,
        };
    }
    classify_retry(error, facts)
}

/// Large-input activation is distinct from the ordinary transient retry list:
/// every 5xx may benefit, but terminal reception and deferred
/// Retry-After still prohibit an automatic new request.
pub(crate) fn eligible_trim_failure(error: &ModelError, facts: &ResponseFacts) -> bool {
    if facts.terminal_received
        || error.is_context_limit()
        || facts
            .retry_after
            .is_some_and(|delay| delay > MAX_RETRY_AFTER)
        || facts
            .http_observation
            .is_some_and(|http| http.phase == super::http::HttpPhase::BeforeBodySource)
    {
        return false;
    }
    let server_failure = facts
        .status
        .is_some_and(|status| (500..600).contains(&status));
    match error.kind.as_str() {
        "http" | "provider" => server_failure,
        "transport" | "incomplete" => {
            server_failure
                || (facts.http_observation.is_some()
                    && facts
                        .status
                        .is_none_or(|status| (200..300).contains(&status)))
        }
        "timeout" => {
            server_failure
                || (facts
                    .http_observation
                    .is_some_and(|http| http.phase != super::http::HttpPhase::BodyReceive)
                    && facts
                        .status
                        .is_none_or(|status| (200..300).contains(&status)))
        }
        _ => false,
    }
}

#[derive(Default)]
pub(crate) struct RetryBudget {
    pub(crate) turn: String,
    pub(crate) generation: i64,
    pub(crate) used: u32,
    posted: bool,
    backoff: Option<Duration>,
    retry_wait_ms: u64,
    pub(crate) usage_limit_retry_fact: Option<super::usage_limit::UsageLimit>,
    /// #182: the one resend after a thinking-binding rejection was used.
    pub(crate) thinking_binding_retried: bool,
}
impl RetryBudget {
    pub(crate) fn observe(&mut self, turn: &str, generation: i64) {
        if self.turn != turn || self.generation != generation {
            self.turn = turn.into();
            self.generation = generation;
            self.used = 0;
            self.posted = false;
            self.backoff = None;
            self.retry_wait_ms = 0;
            self.usage_limit_retry_fact = None;
            self.thinking_binding_retried = false;
        }
    }
    pub(crate) fn usage_limit_retry_bypass(&self) -> Option<super::usage_limit::UsageLimit> {
        self.usage_limit_retry_fact
    }
    pub(crate) fn abandon_retry(&mut self) {
        self.backoff = None;
        self.retry_wait_ms = 0;
        self.clear_usage_limit_retry_bypass();
    }
    pub(crate) fn clear_usage_limit_retry_bypass(&mut self) {
        self.usage_limit_retry_fact = None;
    }
    /// Reset only after the completed response has been durably adopted.
    pub(crate) fn succeeded(&mut self) {
        self.used = 0;
        self.posted = false;
        self.backoff = None;
        self.retry_wait_ms = 0;
        self.thinking_binding_retried = false;
        self.clear_usage_limit_retry_bypass();
    }
    pub(crate) fn decision(
        &self,
        kind: AttemptKind,
        connection: &crate::config::Connection,
        failure: &AttemptFailure,
    ) -> (RetryPolicy, Option<Duration>) {
        let limit = connection.response_max_retries;
        let cap = Duration::from_millis(connection.response_retry_max_delay_ms);
        let retry_after = failure.facts.retry_after;
        let policy = classify_attempt_retry(failure, kind);
        // Do not retry a confirmed reset beyond the remaining finite schedule.
        let policy = if failure.error.usage_limit.as_deref().is_some_and(|fact| {
            u64::try_from(
                fact.resets_at
                    .saturating_sub(chrono::Utc::now().timestamp()),
            )
            .is_ok_and(|seconds| {
                Duration::from_secs(seconds) > self.planned_wait(limit, cap, retry_after)
            })
        }) {
            RetryPolicy::Ineligible
        } else {
            policy
        };
        let delay = if matches!(policy, RetryPolicy::Retry { .. }) {
            self.delay(limit, cap, retry_after).map(|delay| {
                if kind == AttemptKind::Summary && failure.facts.status == Some(429) {
                    // Summary lets the shared rate gate apply the cooldown.
                    Duration::ZERO
                } else {
                    delay
                }
            })
        } else {
            None
        };
        (policy, delay)
    }

    fn charge(&mut self, limit: u32) -> Option<PostCharge<'_>> {
        let previous = (self.posted, self.used);
        if self.posted {
            if self.used >= limit {
                return None;
            }
            self.used += 1;
        }
        self.posted = true;
        Some(PostCharge {
            budget: self,
            previous,
            sent: false,
        })
    }

    pub(crate) fn delay(
        &self,
        limit: u32,
        cap: Duration,
        retry_after: Option<Duration>,
    ) -> Option<Duration> {
        (self.used < limit).then(|| {
            retry_after
                .unwrap_or_else(|| Duration::from_millis(1_000u64 << self.used.min(20)).min(cap))
        })
    }
    pub(crate) fn planned_wait(
        &self,
        limit: u32,
        cap: Duration,
        retry_after: Option<Duration>,
    ) -> Duration {
        (self.used..limit).fold(Duration::ZERO, |total, used| {
            let backoff = Duration::from_millis(1_000u64 << used.min(20)).min(cap);
            total.saturating_add(if used == self.used {
                retry_after.unwrap_or(backoff)
            } else {
                backoff
            })
        })
    }
}

struct PostCharge<'a> {
    budget: &'a mut RetryBudget,
    previous: (bool, u32),
    sent: bool,
}
impl Drop for PostCharge<'_> {
    fn drop(&mut self) {
        if !self.sent {
            (self.budget.posted, self.budget.used) = self.previous;
        }
    }
}

impl AttemptKind {
    fn key(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Summary => "summary",
        }
    }
}

pub(crate) struct AttemptScope {
    pub kind: AttemptKind,
    pub thread: String,
    pub turn: Option<String>,
    pub generation: i64,
    pub selection_revision: i64,
    pub reasoning_epoch: i64,
}

pub(crate) struct AttemptAdmission {
    pub scope: AttemptScope,
    pub profile: String,
    pub model: String,
    pub effort: Option<String>,
    pub manifest: serde_json::Value,
    pub started_at: String,
}

pub(crate) fn admit_attempt(
    db: &rusqlite::Connection,
    id: &str,
    row: AttemptAdmission,
) -> Result<(), StorageError> {
    let scope = row.scope;
    db.execute("INSERT INTO model_attempt(id,kind,thread_id,turn_id,generation,selection_revision,profile_id,model_id,effort,reasoning_epoch,state,input_manifest_json,started_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'inflight',?11,?12)", rusqlite::params![id,scope.kind.key(),scope.thread,scope.turn,scope.generation,scope.selection_revision,row.profile,row.model,row.effort,scope.reasoning_epoch,row.manifest.to_string(),row.started_at])?;
    Ok(())
}

pub(crate) async fn resolve_rate_reservation(
    client: &super::http::HttpModelClient,
    reservation: Result<super::http::RateReservation, ModelError>,
    cancel: &CancellationToken,
    bypass: Option<super::usage_limit::UsageLimit>,
) -> Result<(RateAdmission, u64), ModelError> {
    use super::http::RateReservation;
    match reservation? {
        RateReservation::Ready(lease) => Ok((lease, 0)),
        RateReservation::Waiting(waiter) => {
            let start = tokio::time::Instant::now();
            let result = waiter.wait(cancel).await;
            let admission = client.admission_after_wait(result, bypass)?;
            Ok((
                admission,
                start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            ))
        }
    }
}

pub(crate) type Qualify = Box<
    dyn FnOnce(&rusqlite::Transaction<'_>, &mut AttemptAdmission) -> Result<bool, StorageError>
        + Send,
>;
pub(crate) type LinkAdmission =
    Box<dyn FnOnce(&rusqlite::Transaction<'_>, &str) -> Result<(), StorageError> + Send>;

pub(crate) struct PreparedInput<T> {
    pub payload: serde_json::Value,
    pub context: T,
    pub admission: AttemptAdmission,
    pub qualify: Qualify,
    pub link: Option<LinkAdmission>,
}

pub(crate) struct FailureDecision {
    pub policy: RetryPolicy,
    pub used: u32,
    pub limit: u32,
    pub drop_reasoning: bool,
}

pub(crate) enum Execution<T> {
    Finished {
        attempt: String,
        outcome: Box<RequestOutcome>,
        context: T,
        failure_decision: Option<FailureDecision>,
    },
    AdaptationSuggested {
        attempt: String,
        failure: Box<AttemptFailure>,
        decision: FailureDecision,
    },
    InputChanged,
    Fenced,
    BudgetExhausted,
    RunCeilingReached,
}

pub(crate) type Observer<'a> = Box<dyn FnMut(super::http::ObservedEvent) + Send + 'a>;

pub(crate) trait RequestCaller: Send {
    type Context: Send;
    fn pause<'a, T: Send + 'a>(
        &'a mut self,
        cancel: &'a CancellationToken,
        wait: impl Future<Output = T> + Send + 'a,
    ) -> futures_util::future::BoxFuture<'a, Option<T>>;
    fn release_pins(&mut self) {}
    fn prepare(
        &mut self,
        received_rate_wait: bool,
    ) -> impl Future<
        Output = Result<Option<PreparedInput<Self::Context>>, crate::protocol::ApiError>,
    > + Send;
    fn observe<'a>(
        &'a mut self,
        _attempt: &'a str,
    ) -> impl Future<Output = Option<Observer<'a>>> + Send {
        async { None }
    }
    fn received(&mut self, _usage: StandardUsage) {}
    fn attempt_changed(&self) {}
    fn unsuccessful_attempt_committed(
        &mut self,
        _attempt: &str,
        _output_limit: Option<crate::protocol::AttemptOutputLimit>,
    ) {
    }
}

pub(crate) struct RequestScope<'a> {
    pub database: &'a DbWorker,
    pub client: &'a super::http::HttpModelClient,
    pub connection: &'a crate::config::Connection,
    pub kind: AttemptKind,
    pub adapt_after_transient: bool,
    pub run: Option<&'a str>,
}

pub(crate) async fn execute_request<C: RequestCaller>(
    scope: RequestScope<'_>,
    caller: &mut C,
    budget: &mut RetryBudget,
    cancel: &CancellationToken,
    partial_attempts: &mut Vec<String>,
) -> Result<Execution<C::Context>, crate::protocol::ApiError> {
    use super::http::RateReservation;
    use crate::app::storage_error;
    enum Admission {
        Fenced,
        Committed,
        RunCeilingReached,
    }
    use crate::protocol::{ApiError, ErrorCode};
    let mut original = None;
    loop {
        if cancel.is_cancelled() {
            return Ok(Execution::InputChanged);
        }
        let Some(mut charge) = budget.charge(scope.connection.response_max_retries) else {
            return Ok(Execution::BudgetExhausted);
        };
        if let Some(delay) = charge.budget.backoff.take() {
            let wait = wait_backoff(cancel, delay);
            let elapsed = if delay.is_zero() {
                wait.await
            } else {
                caller.pause(cancel, wait).await.unwrap_or(0)
            };
            charge.budget.retry_wait_ms = charge.budget.retry_wait_ms.saturating_add(elapsed);
            if cancel.is_cancelled() {
                return Ok(Execution::InputChanged);
            }
        }
        let bypass = charge.budget.usage_limit_retry_bypass();
        let reservation = scope.client.reserve_rate_admission(bypass);
        let waited = !matches!(&reservation, Ok(RateReservation::Ready(_)));
        if waited {
            caller.release_pins();
        }
        let wait = resolve_rate_reservation(scope.client, reservation, cancel, bypass);
        let rate = if waited {
            let Some(result) = caller.pause(cancel, wait).await else {
                return Ok(Execution::InputChanged);
            };
            result
        } else {
            wait.await
        };
        if cancel.is_cancelled() {
            return Ok(Execution::InputChanged);
        }
        let (admission, wait_ms) = rate.map_err(|error| {
            ApiError::new(ErrorCode::IoFailed, "Connection admission deferred")
                .details(json!({"kind":error.kind}))
        })?;
        let Some(PreparedInput {
            payload,
            context,
            admission: mut row,
            qualify,
            link,
        }) = caller.prepare(waited).await?
        else {
            return Ok(Execution::InputChanged);
        };
        if original.as_ref().is_some_and(|body| body != &payload) {
            return Ok(Execution::InputChanged);
        }
        let thread = row.scope.thread.clone();
        let prepared = scope
            .client
            .prepare_response(payload.clone(), Some(&thread));
        let output_limit = prepared.as_ref().ok().map(|wire| AdmittedOutputLimit {
            resolved: wire.output_limit,
            connection: scope.client.output_limit_connection().to_owned(),
        });
        let saved_output_limit = output_limit.as_ref().map(|limit| json!(limit));
        let attempt = uuid::Uuid::new_v4().to_string();
        let saved = attempt.clone();
        let token = cancel.clone();
        let run = scope.run.map(str::to_owned).filter(|_| prepared.is_ok());
        let committed = scope
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                if token.is_cancelled() || !qualify(&tx, &mut row)? {
                    return Ok(Admission::Fenced);
                }
                if let Some(run) = run {
                    use crate::control::state::{self, RunStopAdmission};
                    use rusqlite::OptionalExtension;
                    let ceiling = tx.query_row(
                        "SELECT max_model_requests,model_requests_used,state='live' FROM run WHERE id=?1 AND thread_id=?2",
                        rusqlite::params![run, row.scope.thread],
                        |r| Ok((r.get::<_, Option<u32>>(0)?, r.get::<_, u32>(1)?, r.get::<_, bool>(2)?)),
                    ).optional()?;
                    match ceiling {
                        None | Some((Some(_), _, false)) => return Ok(Admission::Fenced),
                        // Uncapped Runs keep the existing request qualification,
                        // including a proactive retry after its Run has finished.
                        Some((None, _, _)) => {}
                        Some((Some(ceiling), used, true)) if used == ceiling => {
                            // Qualification may prepare attempt metadata. Do not
                            // commit it for a request that will never be admitted.
                            tx.rollback()?;
                            let tx = db.transaction()?;
                            state::admit_run_stop_tx(&tx, &run, RunStopAdmission::ModelRequestCeilingReached)?;
                            tx.commit()?;
                            return Ok(Admission::RunCeilingReached);
                        }
                        Some((Some(_), _, true)) => {
                            tx.execute("UPDATE run SET model_requests_used=model_requests_used+1 WHERE id=?1", [&run])?;
                        }
                    }
                }
                if let Some(limit) = saved_output_limit {
                    row.manifest["output_limit"] = limit;
                }
                admit_attempt(&tx, &saved, row)?;
                if let Some(link) = link {
                    link(&tx, &saved)?;
                }
                tx.commit()?;
                Ok(Admission::Committed)
            })
            .await
            .map_err(storage_error)?;
        match committed {
            Admission::Fenced => return Ok(Execution::Fenced),
            Admission::RunCeilingReached => return Ok(Execution::RunCeilingReached),
            Admission::Committed => {}
        }
        let retry_wait_ms = std::mem::take(&mut charge.budget.retry_wait_ms);
        let outcome = {
            let send = async {
                let mut observer = caller.observe(&attempt).await;
                let (events, mut receive) = if observer.is_some() {
                    let (tx, rx) = tokio::sync::mpsc::channel(32);
                    (Some(tx), Some(rx))
                } else {
                    (None, None)
                };
                let response =
                    scope
                        .client
                        .response_observed(prepared, cancel.clone(), events, bypass);
                tokio::pin!(response);
                if let (Some(receive), Some(observer)) = (&mut receive, &mut observer) {
                    loop {
                        tokio::select! { result=&mut response=>break result, Some(event)=receive.recv()=>observer(event) }
                    }
                } else {
                    response.await
                }
            };
            send_attempt(cancel, admission, wait_ms, send).await
        };
        let facts = match &outcome {
            Ok(response) => response.response_facts,
            Err(failure) => failure.facts,
        };
        charge.sent = facts
            .http_observation
            .is_some_and(|http| http.network_started);
        drop(charge);
        caller.release_pins();
        let Err(failure) = &outcome else {
            let raw = &outcome.as_ref().expect("successful outcome").raw;
            let state = if cancel.is_cancelled() {
                "cancelled"
            } else {
                "completed"
            };
            let usage = persist_outcome(
                scope.database,
                &attempt,
                state,
                json!({"raw_response":raw}),
                facts,
                retry_wait_ms,
                cancel,
            )
            .await
            .map_err(storage_error)?;
            caller.received(usage);
            return Ok(Execution::Finished {
                attempt,
                outcome: Box::new(outcome),
                context,
                failure_decision: None,
            });
        };
        let (policy, delay) = budget.decision(scope.kind, scope.connection, failure);
        let can_retry = !cancel.is_cancelled()
            && facts
                .http_observation
                .is_some_and(|http| http.network_started);
        let delay = delay.filter(|_| can_retry);
        let limit = scope.connection.response_max_retries;
        let binding = scope.kind == AttemptKind::Conversation
            && can_retry
            && failure.error.is_thinking_binding()
            && !budget.thinking_binding_retried
            && budget.used < limit;
        let decision = FailureDecision {
            policy,
            used: budget.used,
            limit,
            drop_reasoning: binding,
        };
        if binding {
            budget.thinking_binding_retried = true;
        }
        let adapt = scope.kind == AttemptKind::Conversation
            && delay.is_some()
            && (failure.error.is_context_limit()
                || (scope.adapt_after_transient
                    && budget.used >= 1
                    && eligible_trim_failure(&failure.error, &failure.facts)));
        {
            let partial = delay.is_some()
                && matches!(
                    policy,
                    RetryPolicy::Retry {
                        partial_output: true
                    }
                );
            let mut body = failure_body(&attempt, failure);
            if let Some(wait) = delay.or(binding.then_some(Duration::ZERO)) {
                body["automatic_retry"] = json!({"number":budget.used+1,"limit":limit,"delay_ms":wait.as_millis() as u64,"partial_response_discarded":partial,"possible_duplicate_charge":!binding});
                if binding {
                    body["automatic_retry"]["reason"] = json!("thinking_binding");
                }
            } else {
                if let RetryPolicy::Deferred(wait) = policy {
                    body["error"]["message"] = json!(format!(
                        "{}; server requested Retry-After {}s; automatic wait limit is 600s",
                        failure.error.message,
                        wait.as_secs()
                    ));
                    body["automatic_retry_deferred"] = json!({"retry_after_ms":wait.as_millis().min(u64::MAX as u128) as u64,"maximum_automatic_wait_ms":600_000});
                } else if matches!(policy, RetryPolicy::Retry { .. })
                    && budget.used >= limit
                    && limit > 0
                {
                    body["automatic_retry_exhausted"] =
                        json!({"retries":budget.used,"limit":limit});
                }
            }
            let cancelled = cancel.is_cancelled();
            let state = if cancelled {
                "cancelled"
            } else if scope.kind == AttemptKind::Summary
                || binding
                || delay.is_some() && !failure.error.is_context_limit()
            {
                "failed"
            } else {
                "incomplete"
            };
            let usage = persist_outcome(
                scope.database,
                &attempt,
                state,
                body,
                facts,
                retry_wait_ms,
                cancel,
            )
            .await
            .map_err(storage_error)?;
            caller.unsuccessful_attempt_committed(
                &attempt,
                output_limit
                    .filter(|_| failure.error.kind == "output_limit")
                    .map(AdmittedOutputLimit::stopped),
            );
            caller.received(usage);
            if partial && !cancelled && scope.kind == AttemptKind::Conversation {
                partial_attempts.push(attempt.clone());
            }
            if delay.is_some() && !cancelled {
                caller.attempt_changed();
            }
        }
        let Some(delay) = delay.filter(|_| !cancel.is_cancelled()) else {
            return Ok(Execution::Finished {
                attempt,
                outcome: Box::new(outcome),
                context,
                failure_decision: Some(decision),
            });
        };
        budget.usage_limit_retry_fact = failure
            .error
            .usage_limit
            .as_deref()
            .filter(|fact| !fact.locally_blocked)
            .copied();
        budget.backoff = Some(delay);
        if adapt {
            let Err(failure) = outcome else {
                unreachable!()
            };
            return Ok(Execution::AdaptationSuggested {
                attempt,
                failure: Box::new(failure),
                decision,
            });
        }
        drop(context);
        original = Some(payload);
    }
}

#[cfg(test)]
pub(crate) fn fail_attempt(
    db: &SqlConnection,
    attempt: &str,
    error: &Value,
    at: &str,
) -> Result<AttemptTransition, StorageError> {
    write_outcome(db, attempt, error, "failed", at, None, None)?;
    Ok(AttemptTransition::Applied)
}

fn write_outcome(
    db: &SqlConnection,
    attempt: &str,
    body: &Value,
    state: &str,
    at: &str,
    rate: Option<Value>,
    http: Option<Value>,
) -> Result<StandardUsage, StorageError> {
    let (body, usage) = normalized_failure(body);
    let diagnostic = diagnostic_json(&body["raw_response"]);
    let patch = json!({"http_observation":http,"rate_observation":rate});
    let error = (state != "completed").then(|| body.to_string());
    let changed = db.execute("UPDATE model_attempt SET state=?2,ended_at=?3,response_id=?8,usage_json=COALESCE(?4,usage_json),
         error_json=?5,
         input_manifest_json=json_set(json_patch(input_manifest_json,?6),'$.prompt_cache_diagnostics',json(CASE WHEN json_extract(input_manifest_json,'$.prompt_cache.diagnostics_enabled')=1 THEN ?7 ELSE 'null' END)) WHERE id=?1",
         params![attempt,state,at,usage.map(|usage| usage.to_value().to_string()),error,patch.to_string(),diagnostic,body.pointer("/raw_response/id").and_then(Value::as_str)])?;
    if changed != 1 {
        return Err(ApiError::new(
            ErrorCode::TargetUnavailable,
            "Model attempt is no longer available",
        )
        .into());
    }
    Ok(usage.unwrap_or_default())
}
fn diagnostic_json(response: &Value) -> String {
    crate::model::cache_diagnostics::bounded(&response["prompt_cache_diagnostics"])
        .unwrap_or(Value::Null)
        .to_string()
}
fn normalized_failure(error: &Value) -> (Value, Option<crate::usage::StandardUsage>) {
    let mut error = error.clone();
    let usage = error
        .pointer("/raw_response/usage")
        .map(crate::usage::StandardUsage::from_value);
    if let Some(raw) = error.get_mut("raw_response") {
        crate::usage::normalize_response(raw);
        crate::model::cache_diagnostics::normalize_response(raw);
    }
    (error, usage)
}

#[cfg(test)]
pub(crate) fn incomplete_attempt(
    db: &SqlConnection,
    attempt: &str,
    error: &Value,
    at: &str,
) -> Result<AttemptTransition, StorageError> {
    write_outcome(db, attempt, error, "incomplete", at, None, None)?;
    Ok(AttemptTransition::Applied)
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::{config::Connection, model::http::HttpModelClient, protocol::ApiError};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    struct Caller(AttemptKind, serde_json::Value, Option<CancellationToken>);
    impl RequestCaller for Caller {
        type Context = ();
        fn pause<'a, T: Send + 'a>(
            &'a mut self,
            _: &'a CancellationToken,
            wait: impl Future<Output = T> + Send + 'a,
        ) -> futures_util::future::BoxFuture<'a, Option<T>> {
            Box::pin(async move { Some(wait.await) })
        }
        async fn observe<'a>(&'a mut self, _: &'a str) -> Option<Observer<'a>> {
            if let Some(token) = self.2.take() {
                token.cancel();
            }
            None
        }
        async fn prepare(&mut self, _: bool) -> Result<Option<PreparedInput<()>>, ApiError> {
            let kind = self.0;
            Ok(Some(PreparedInput {
                payload: self.1.clone(),
                context: (),
                admission: AttemptAdmission {
                    scope: AttemptScope {
                        kind,
                        thread: "thread".into(),
                        turn: (kind == AttemptKind::Conversation).then(|| "turn".into()),
                        generation: 0,
                        selection_revision: 0,
                        reasoning_epoch: 0,
                    },
                    profile: "fixture".into(),
                    model: "fixture".into(),
                    effort: None,
                    manifest: json!({}),
                    started_at: crate::control::state::now(),
                },
                link: None,
                qualify: Box::new(move |tx, _| {
                    if kind == AttemptKind::Summary {
                        return Ok(true);
                    }
                    Ok(tx.query_row(
                        "SELECT selection_revision=0 FROM thread WHERE id='thread'",
                        [],
                        |r| r.get(0),
                    )?)
                }),
            }))
        }
    }

    async fn verify_no_second_post(kind: AttemptKind, before_dispatch: Option<&str>) {
        crate::test_watchdog::watchdog("executor gates repeated POST", async {
            let root = tempfile::tempdir().unwrap();
            let db = DbWorker::open(root.path().join("fixture.sqlite")).await.unwrap();
            db.call(|db| {
                db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('thread','/','','fixture','fixture','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('run','thread','live','now'); INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('turn','thread','run','running','now');")?;
                Ok(())
            }).await.unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let connection = Connection { base_url:format!("http://{}/v1",listener.local_addr().unwrap()), response_max_retries: u32::from(kind == AttemptKind::Conversation), response_retry_max_delay_ms:1, ..Default::default() };
            let client = HttpModelClient::for_connection("fixture",connection.clone()).unwrap();
            let scope = || RequestScope { database:&db, client:&client, connection:&connection, kind, adapt_after_transient:false, run:None };
            let mut caller = Caller(kind,json!({"model":"fixture","input":[{"role":"user","content":"full"}]}), None);
            let cancel = CancellationToken::new();
            let mut budget = RetryBudget::default();
            let mut audits = Vec::new();
            if let Some(mode) = before_dispatch {
                let local_cancel = CancellationToken::new();
                let valid = caller.1.clone();
                if mode == "cancelled" { caller.2 = Some(local_cancel.clone()); }
                else { caller.1[crate::model::THREAD_OUTPUT_LIMIT_FIELD] = json!(0); }
                let result = execute_request(scope(), &mut caller, &mut budget, &local_cancel, &mut audits).await.unwrap();
                let Execution::Finished { outcome, .. } = result else { panic!("expected pre-dispatch failure"); };
                assert_eq!(outcome.unwrap_err().error.kind, mode);
                assert!(!budget.posted);
                assert_eq!(budget.used, 0);
                caller.1 = valid;
            }
            let provider = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|part|part == b"\r\n\r\n") {
                    let mut bytes = [0;1024];
                    let n = stream.read(&mut bytes).await.unwrap(); assert!(n>0); request.extend_from_slice(&bytes[..n]);
                }
                assert!(request.starts_with(b"POST "));
                if kind == AttemptKind::Conversation {
                    db.call(|db| { db.execute("UPDATE thread SET selection_revision=1 WHERE id='thread'",[])?; Ok(()) }).await.unwrap();
                }
                let (status, code) = if kind == AttemptKind::Summary { (400, "context_length_exceeded") } else { (503, "server_error") };
                let body = json!({"error":{"code":code,"message":"fixture"}}).to_string();
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            };
            let (first, ()) = tokio::join!(execute_request(scope(), &mut caller, &mut budget, &cancel, &mut audits), provider);
            match kind {
                AttemptKind::Summary => {
                    let Execution::Finished { outcome, .. } = first.unwrap() else { panic!("expected input refusal"); };
                    assert!(outcome.unwrap_err().error.is_context_limit());
                    caller.1["input"][0]["content"] = json!("reduced");
                    assert!(matches!(execute_request(scope(),&mut caller,&mut budget,&cancel,&mut audits).await.unwrap(), Execution::BudgetExhausted));
                }
                AttemptKind::Conversation => assert!(matches!(first.unwrap(),Execution::Fenced)),
            }
            assert_eq!(db.call(|db|Ok(db.query_row("SELECT count(*) FROM model_attempt",[],|r|r.get::<_,i64>(0))?)).await.unwrap(),1 + i64::from(before_dispatch.is_some()));
            db.shutdown().await.unwrap();
        }).await.unwrap();
    }

    #[tokio::test]
    async fn reduced_summary_cannot_post_after_scope_budget_is_exhausted() {
        verify_no_second_post(AttemptKind::Summary, None).await;
    }

    #[tokio::test]
    async fn stale_conversation_resend_is_fenced_before_post() {
        verify_no_second_post(AttemptKind::Conversation, None).await;
    }
    #[tokio::test]
    async fn local_rejection_does_not_charge_a_post() {
        verify_no_second_post(AttemptKind::Summary, Some("protocol")).await;
    }

    #[tokio::test]
    async fn cancellation_after_admission_before_dispatch_does_not_charge_a_post() {
        verify_no_second_post(AttemptKind::Summary, Some("cancelled")).await;
    }
}
