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

/// Synchronous fixtures use the same deadlock budget, never a speed assertion.
#[allow(dead_code)]
pub struct Deadline {
    started: std::time::Instant,
    waiting_for: &'static str,
}
#[allow(dead_code)]
impl Deadline {
    pub fn new(waiting_for: &'static str) -> Self {
        Self {
            started: std::time::Instant::now(),
            waiting_for,
        }
    }
    pub fn expired(&self) -> bool {
        self.started.elapsed() >= WATCHDOG
    }
    #[track_caller]
    pub fn check(&self) {
        assert!(
            !self.expired(),
            "test watchdog: waiting for {}",
            self.waiting_for
        );
    }
}

#[allow(dead_code)]
#[track_caller]
pub fn blocking_recv<T>(
    waiting_for: &'static str,
    receiver: &std::sync::mpsc::Receiver<T>,
) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
    let result = receiver.recv_timeout(WATCHDOG);
    if result.is_err() {
        eprintln!(
            "test watchdog/channel closed: waiting for {waiting_for} at {}",
            Location::caller()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_only_guard_budget_is_sixty_seconds() {
        assert_eq!(WATCHDOG.as_secs(), 60);
    }
    #[tokio::test(start_paused = true)]
    async fn shared_guard_expires_only_on_the_controlled_clock() {
        let future = watchdog(
            "deliberately pending virtual fixture",
            std::future::pending::<()>(),
        );
        tokio::pin!(future);
        assert!(futures_util::poll!(&mut future).is_pending());
        tokio::time::advance(WATCHDOG - Duration::from_nanos(1)).await;
        assert!(futures_util::poll!(&mut future).is_pending());
        tokio::time::advance(Duration::from_nanos(1)).await;
        assert!(future.await.is_err());
    }
    #[tokio::test]
    async fn shared_guard_preserves_ready_success_and_failure_facts() {
        assert_eq!(
            watchdog("ready value", std::future::ready(41))
                .await
                .unwrap(),
            41
        );
        assert_eq!(
            watchdog(
                "domain failure",
                std::future::ready(Err::<(), _>("saved fact"))
            )
            .await
            .unwrap(),
            Err("saved fact")
        );
    }
}
