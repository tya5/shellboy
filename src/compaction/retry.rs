//! #224 stage 3: summary requests retry transient failures through the profile
//! gate, at most `response_max_retries` times per request. Nothing here adopts.
use super::*;
use crate::model::{
    http::AttemptFailure,
    retry_class::{RetryPolicy, classify_retry},
};
use std::time::Duration;

/// The wait before resending after `failure`, or `None` when it must not be
/// resent. Uses the shared classifier, not the conversation SSE exception.
/// A usage limit is never retried,
/// and a context limit is split, not resent unchanged. After a 429 the gate's
/// cooldown is the wait; otherwise the conversation schedule applies.
pub(super) fn retry_delay(
    connection: &crate::config::Connection,
    failure: &AttemptFailure,
    retries: u32,
) -> Option<Duration> {
    if failure.error.usage_limit.is_some()
        || failure.error.is_context_limit()
        || retries >= connection.response_max_retries
    {
        return None;
    }
    let policy = classify_retry(&failure.error, &failure.facts);
    if !matches!(policy, RetryPolicy::Retry { .. }) {
        return None;
    }
    if failure.facts.status == Some(429) {
        return Some(Duration::ZERO);
    }
    let cap = Duration::from_millis(connection.response_retry_max_delay_ms);
    Some(
        failure
            .facts
            .retry_after
            .unwrap_or_else(|| Duration::from_millis(1_000u64 << retries.min(20)).min(cap)),
    )
}

fn cancelled() -> ApiError {
    invalid("Compaction cancelled; original path retained")
}

impl Compactor {
    /// Waits `delay` cancellably with the Thread boundary released when owned.
    pub(super) async fn summary_backoff(
        &self,
        owner: &mut super::single_request::DispatchOwner<'_, '_>,
        delay: Duration,
        cancel: &CancellationToken,
    ) -> Result<(), ApiError> {
        if delay.is_zero() {
            return Ok(());
        }
        match owner {
            super::single_request::DispatchOwner::Manual { boundary } => {
                let wait = async {
                    tokio::select! {biased;_=cancel.cancelled()=>{},_=tokio::time::sleep(delay)=>{}}
                };
                if let Some(boundary) = boundary.as_deref_mut() {
                    boundary
                        .pause(cancel, wait)
                        .await
                        .map_err(|_| cancelled())?;
                } else {
                    wait.await;
                }
            }
            super::single_request::DispatchOwner::Gc => {
                tokio::select! {biased;_=cancel.cancelled()=>{},_=tokio::time::sleep(delay)=>{}};
            }
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        Ok(())
    }
}
