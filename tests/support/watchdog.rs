// Included by the lib test harness and each integration-test binary.
use std::{future::Future, panic::Location, time::Duration};

pub const WATCHDOG: Duration = Duration::from_secs(60);

/// Deadlock protection only. Callers still decide success from the returned facts.
#[track_caller]
pub fn watchdog<F: Future>(
    waiting_for: &'static str,
    future: F,
) -> impl Future<Output = Result<F::Output, tokio::time::error::Elapsed>> {
    let caller = Location::caller();
    async move {
        let result = tokio::time::timeout(WATCHDOG, future).await;
        if result.is_err() {
            eprintln!(
                "test watchdog: waiting for {waiting_for} at {}:{}; inspect saved progress diagnostics (not a latency guarantee)",
                caller.file(),
                caller.line()
            );
        }
        result
    }
}
