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
    /// Pass the same safety budget to APIs that require an absolute deadline.
    pub fn at(&self) -> std::time::Instant {
        self.started + WATCHDOG
    }
    /// Wait for the next fixture event within this one shared safety budget.
    #[track_caller]
    pub fn receive<T>(
        &self,
        receiver: &std::sync::mpsc::Receiver<T>,
    ) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
        let result = receiver.recv_timeout(
            self.at()
                .saturating_duration_since(std::time::Instant::now()),
        );
        if result.is_err() {
            eprintln!(
                "test watchdog/channel closed: waiting for {} at {}",
                self.waiting_for,
                Location::caller()
            );
        }
        result
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

/// Wait for a concrete channel state using the caller's unchanged safety budget.
/// The supported frontend fixtures use a multi-thread Tokio runtime.
#[allow(dead_code)]
pub async fn receive_until<T, R>(
    waiting_for: &'static str,
    receiver: &std::sync::mpsc::Receiver<T>,
    budget: Duration,
    mut select: impl FnMut(T) -> Option<R>,
) -> R {
    let deadline = std::time::Instant::now() + budget;
    let mut observed = 0usize;
    loop {
        let value = tokio::task::block_in_place(|| {
            receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        })
        .unwrap_or_else(|error| panic!("{waiting_for}: {error}; observed {observed} updates"));
        observed += 1;
        if let Some(result) = select(value) {
            return result;
        }
    }
}

/// Keep the bounded UI consumer alive until the real frontend task terminates.
/// The caller keeps its existing timeout; no delay or time-success test lives here.
#[allow(dead_code)]
pub async fn drain_to_completion<T, R>(
    mut task: tokio::task::JoinHandle<R>,
    receiver: &std::sync::mpsc::Receiver<T>,
) -> Result<R, tokio::task::JoinError> {
    loop {
        for _ in 0..128 {
            if receiver.try_recv().is_err() {
                break;
            }
        }
        tokio::select! {result=&mut task=>return result,_=tokio::task::yield_now()=>{}}
    }
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn state_receiver_observes_the_matching_fact_not_a_poll_count() {
        let (send, receive) = std::sync::mpsc::sync_channel(4);
        for value in [1, 2, 3] {
            send.send(value).unwrap();
        }
        let result = receive_until("third fact", &receive, Duration::from_secs(5), |value| {
            (value == 3).then_some(value)
        })
        .await;
        assert_eq!(result, 3);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_consumer_drains_bounded_late_results_until_producer_exits() {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        send.send(0).unwrap();
        let producer = tokio::task::spawn_blocking(move || {
            for value in 1..=3 {
                send.send(value).unwrap();
            }
            3
        });
        let result = watchdog(
            "bounded producer completion",
            drain_to_completion(producer, &receive),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, 3);
    }
}
