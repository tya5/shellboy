//! #224 stage 3: summary requests retry transient failures inside the existing
//! map/reduce request budgets, through the profile gate. Nothing here adopts.
use super::*;
use crate::model::{
    http::AttemptFailure,
    retry_class::{RetryPolicy, classify_retry},
};
use std::time::Duration;

/// The wait before resending after `failure`, or `None` when it must not be
/// resent. Uses the conversation classifier. A usage limit is never retried,
/// and a context limit is split, not resent unchanged. After a 429 the gate's
/// cooldown is the wait; otherwise the conversation schedule applies.
pub(super) fn retry_delay(
    connection: &crate::config::Connection,
    failure: &AttemptFailure,
    retries: u32,
) -> Option<Duration> {
    if failure.error.usage_limit.is_some() || failure.error.is_context_limit() {
        return None;
    }
    let policy = classify_retry(
        &failure.error,
        &failure.facts,
        connection.response_retry_partial,
    );
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
    /// Waits `delay` (cancellable) with the Thread boundary released, then
    /// revalidates the snapshot, as an admission wait does.
    pub(super) async fn summary_backoff(
        &self,
        snapshot: &Snapshot,
        delay: Duration,
        cancel: &CancellationToken,
        boundary: Option<&mut boundary::Boundary<'_>>,
    ) -> Result<(), ApiError> {
        if delay.is_zero() {
            return Ok(());
        }
        let wait = async {
            tokio::select! { biased; _ = cancel.cancelled() => {}, _ = tokio::time::sleep(delay) => {} }
        };
        if let Some(boundary) = boundary {
            boundary
                .pause(cancel, wait)
                .await
                .map_err(|_| cancelled())?;
            let witness = snapshot.clone();
            self.database
                .call(move |db| {
                    let tx = db.transaction()?;
                    validate(&tx, &witness)?;
                    tx.commit()?;
                    Ok(())
                })
                .await
                .map_err(storage_error)?;
        } else {
            wait.await;
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        Ok(())
    }
}
