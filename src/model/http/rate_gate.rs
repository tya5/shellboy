//! Ephemeral, profile-scoped FIFO admission for shared 429 cooldowns.
use super::{ModelError, cancelled};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};
use tokio_util::sync::CancellationToken;

const MAX_WAIT: Duration = Duration::from_secs(600);
const MAX_WAITERS: usize = 1024;
#[derive(Default)]
struct Feedback {
    until: Option<Instant>,
    beyond_clock: bool,
    generation: u64,
    consecutive: u32,
    counted_generation: Option<u64>,
    next_ticket: u64,
    queued: VecDeque<u64>,
    probe: Option<u64>,
}
#[derive(Clone)]
pub(super) struct RateGate {
    feedback: Arc<Mutex<Feedback>>,
    changed: Arc<Notify>,
    max_backoff: Duration,
}
pub(crate) struct Lease {
    _permit: Option<u64>,
    generation: u64,
    gate: RateGate,
}
pub(super) enum Reservation {
    Ready(Lease),
    Waiting(Waiter),
}
pub(crate) struct Waiter {
    id: Option<u64>,
    gate: RateGate,
    limit: Instant,
}
impl Drop for Waiter {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let mut state = self.gate.feedback.lock().expect("rate gate poisoned");
            if let Some(index) = state.queued.iter().position(|item| *item == id) {
                state.queued.remove(index);
            }
            drop(state);
            self.gate.changed.notify_waiters();
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(token) = self._permit {
            let mut state = self.gate.feedback.lock().expect("rate gate poisoned");
            // A probe released at response headers must not clear a later probe.
            if state.probe == Some(token) {
                state.probe = None;
            }
            drop(state);
            self.gate.changed.notify_waiters();
        }
    }
}
impl RateGate {
    pub(super) fn new(max_backoff_ms: u64) -> Self {
        Self {
            feedback: Arc::new(Mutex::new(Feedback::default())),
            changed: Arc::new(Notify::new()),
            max_backoff: Duration::from_millis(max_backoff_ms),
        }
    }
    pub(super) fn reserve(&self) -> Result<Reservation, ModelError> {
        let mut state = self.feedback.lock().expect("rate gate poisoned");
        if state.beyond_clock {
            return Err(ModelError::new(
                "rate_limit_deferred",
                "Connection cooldown exceeds the monotonic clock range",
            ));
        }
        if state.until.is_none() && state.probe.is_none() && state.queued.is_empty() {
            return Ok(Reservation::Ready(Lease {
                _permit: None,
                generation: state.generation,
                gate: self.clone(),
            }));
        }
        if state.queued.len() >= MAX_WAITERS {
            return Err(ModelError::new(
                "rate_limit_deferred",
                "Connection admission queue is full",
            ));
        }
        let id = state.next_ticket;
        state.next_ticket = state.next_ticket.checked_add(1).ok_or_else(|| {
            ModelError::new(
                "rate_limit_deferred",
                "Connection admission ticket overflow",
            )
        })?;
        state.queued.push_back(id);
        Ok(Reservation::Waiting(Waiter {
            id: Some(id),
            gate: self.clone(),
            limit: Instant::now() + MAX_WAIT,
        }))
    }
    pub(super) async fn acquire(&self, cancel: &CancellationToken) -> Result<Lease, ModelError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        match self.reserve()? {
            Reservation::Ready(lease) => Ok(lease),
            Reservation::Waiting(waiter) => waiter.wait(cancel).await,
        }
    }
    pub(super) fn generation(&self) -> u64 {
        self.feedback.lock().expect("rate gate poisoned").generation
    }
    /// A success status at response headers ends the probe: clear the cooldown
    /// generation and release the probe permit without waiting for the body.
    /// Stale successes (older generation) and non-probe requests do nothing.
    pub(super) fn probe_succeeded(&self, request_generation: u64) {
        let mut state = self.feedback.lock().expect("rate gate poisoned");
        if state.probe.is_none() || state.generation != request_generation {
            return;
        }
        state.probe = None;
        state.consecutive = 0;
        state.counted_generation = None;
        state.until = None;
        drop(state);
        self.changed.notify_waiters();
    }
    pub(super) fn throttle(&self, header: Option<Duration>, request_generation: u64) {
        let mut state = self.feedback.lock().expect("rate gate poisoned");
        let new_wave = state
            .counted_generation
            .is_none_or(|last| request_generation > last);
        let exponent = if new_wave {
            state.consecutive
        } else {
            state.consecutive.saturating_sub(1)
        };
        let factor = 1u64 << exponent.min(20);
        let fallback = Duration::from_secs(factor).min(self.max_backoff);
        let delay = header
            .unwrap_or(fallback)
            .max(fallback.min(Duration::from_secs(1)));
        if let Some(until) = Instant::now().checked_add(delay) {
            state.until = Some(state.until.map_or(until, |old| old.max(until)));
        } else {
            // An unrepresentable provider delay must not become an early retry.
            state.beyond_clock = true;
        }
        if new_wave {
            state.consecutive = state.consecutive.saturating_add(1);
            state.counted_generation = Some(request_generation);
        }
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.changed.notify_waiters();
    }
}
impl Waiter {
    pub(crate) async fn wait(mut self, cancel: &CancellationToken) -> Result<Lease, ModelError> {
        loop {
            let changed = self.gate.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let wake_at = {
                let mut state = self.gate.feedback.lock().expect("rate gate poisoned");
                if state.beyond_clock || state.until.is_some_and(|until| until > self.limit) {
                    return Err(ModelError::new(
                        "rate_limit_deferred",
                        "Connection cooldown exceeds the automatic wait limit",
                    ));
                }
                let now = Instant::now();
                if now >= self.limit {
                    return Err(ModelError::new(
                        "rate_limit_deferred",
                        "Connection admission exceeded the automatic wait limit",
                    ));
                }
                if state.queued.front() == self.id.as_ref()
                    && state.probe.is_none()
                    && state.until.is_none_or(|until| until <= now)
                {
                    state.queued.pop_front();
                    self.id = None;
                    let generation = state.generation;
                    if state.until.is_none() {
                        // The probe succeeded: drain queued admissions in order
                        // without serializing their subsequent HTTP requests.
                        drop(state);
                        self.gate.changed.notify_waiters();
                        return Ok(Lease {
                            _permit: None,
                            generation,
                            gate: self.gate.clone(),
                        });
                    }
                    let token = state.next_ticket;
                    state.next_ticket = state.next_ticket.wrapping_add(1);
                    state.probe = Some(token);
                    return Ok(Lease {
                        _permit: Some(token),
                        generation,
                        gate: self.gate.clone(),
                    });
                }
                state
                    .until
                    .filter(|until| *until > now)
                    .map_or(self.limit, |until| until.min(self.limit))
            };
            tokio::select! { biased; _ = cancel.cancelled() => return Err(cancelled()), _ = &mut changed => {}, _ = tokio::time::sleep_until(wake_at) => {} }
        }
    }
}
impl Lease {
    pub(crate) fn complete(&self, status: u16) {
        if status == 429 || self._permit.is_none() {
            return;
        }
        let mut state = self.gate.feedback.lock().expect("rate gate poisoned");
        if state.generation == self.generation {
            state.consecutive = 0;
            state.counted_generation = None;
            state.until = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn completed_non_429_probe_clears_cooldown_without_serializing_new_requests() {
        let gate = RateGate::new(60_000);
        gate.throttle(None, gate.generation());
        tokio::time::advance(Duration::from_secs(1)).await;
        let lease = gate.acquire(&CancellationToken::new()).await.unwrap();
        lease.complete(500);
        drop(lease);
        assert!(matches!(gate.reserve(), Ok(Reservation::Ready(_))));
    }
    #[tokio::test(start_paused = true)]
    async fn probe_success_at_headers_admits_waiters_while_the_body_streams() {
        let gate = RateGate::new(60_000);
        let token = CancellationToken::new();
        gate.throttle(Some(Duration::from_secs(2)), gate.generation());
        let generation = gate.generation();
        let Reservation::Waiting(first) = gate.reserve().unwrap() else {
            panic!("expected waiter")
        };
        let Reservation::Waiting(second) = gate.reserve().unwrap() else {
            panic!("expected waiter")
        };
        let queued = tokio::spawn({
            let token = token.clone();
            async move { second.wait(&token).await }
        });
        tokio::time::advance(Duration::from_secs(2)).await;
        // The probe holds its lease for the whole (long) streamed response.
        let probe = first.wait(&token).await.unwrap();
        assert!(probe._permit.is_some());
        tokio::task::yield_now().await;
        assert!(!queued.is_finished());
        // A stale generation's success changes nothing.
        gate.probe_succeeded(generation.wrapping_sub(1));
        tokio::task::yield_now().await;
        assert!(!queued.is_finished());
        // Success headers arrive; the body continues far past the waiter limit.
        gate.probe_succeeded(generation);
        let admitted = queued.await.unwrap().expect("queued request is admitted");
        assert!(admitted._permit.is_none(), "no second probe is needed");
        tokio::time::advance(MAX_WAIT + Duration::from_secs(60)).await;
        assert!(matches!(gate.reserve(), Ok(Reservation::Ready(_))));
        // Dropping the long-lived probe does not disturb a later cooldown.
        gate.throttle(Some(Duration::from_secs(5)), gate.generation());
        drop(probe);
        assert!(matches!(gate.reserve(), Ok(Reservation::Waiting(_))));
        drop(admitted);
    }
    #[tokio::test(start_paused = true)]
    async fn dropped_probe_keeps_cooldown_and_unblocks_next_waiter() {
        let gate = RateGate::new(60_000);
        gate.throttle(Some(Duration::from_secs(2)), gate.generation());
        let token = CancellationToken::new();
        tokio::time::advance(Duration::from_secs(2)).await;
        let probe = gate.acquire(&token).await.unwrap();
        let waiter = tokio::spawn({
            let gate = gate.clone();
            let token = token.clone();
            async move { gate.acquire(&token).await.unwrap() }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(probe);
        let next = waiter.await.unwrap();
        assert!(next._permit.is_some());
        assert!(gate.feedback.lock().unwrap().until.is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn synchronous_reservations_stay_fifo_when_polled_in_reverse_order() {
        let gate = RateGate::new(60_000);
        gate.throttle(Some(Duration::from_secs(1)), gate.generation());
        let Reservation::Waiting(first) = gate.reserve().unwrap() else {
            panic!("expected waiter")
        };
        let Reservation::Waiting(second) = gate.reserve().unwrap() else {
            panic!("expected waiter")
        };
        let later =
            tokio::spawn(async move { second.wait(&CancellationToken::new()).await.unwrap() });
        tokio::task::yield_now().await;
        let earlier =
            tokio::spawn(async move { first.wait(&CancellationToken::new()).await.unwrap() });
        tokio::time::advance(Duration::from_secs(1)).await;
        let probe = earlier.await.unwrap();
        assert!(!later.is_finished());
        drop(probe);
        drop(later.await.unwrap());
    }
    #[tokio::test(start_paused = true)]
    async fn cancelling_the_oldest_waiter_unblocks_the_next_ticket() {
        let gate = RateGate::new(60_000);
        gate.throttle(None, gate.generation());
        let Reservation::Waiting(first) = gate.reserve().unwrap() else {
            panic!("expected waiter")
        };
        let Reservation::Waiting(second) = gate.reserve().unwrap() else {
            panic!("expected waiter")
        };
        drop(first);
        tokio::time::advance(Duration::from_secs(1)).await;
        let lease = second.wait(&CancellationToken::new()).await.unwrap();
        assert!(lease._permit.is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn bounded_queue_recovers_capacity_when_a_waiter_is_dropped() {
        let gate = RateGate::new(60_000);
        gate.throttle(None, gate.generation());
        let mut tickets = Vec::new();
        for _ in 0..MAX_WAITERS {
            let Reservation::Waiting(waiter) = gate.reserve().unwrap() else {
                panic!("expected waiter")
            };
            tickets.push(waiter);
        }
        assert!(gate.reserve().is_err());
        drop(tickets.pop());
        assert!(matches!(gate.reserve(), Ok(Reservation::Waiting(_))));
    }
    #[tokio::test(start_paused = true)]
    async fn ordinary_requests_do_not_serialize_without_feedback() {
        let gate = RateGate::new(60_000);
        let token = CancellationToken::new();
        let first = gate.acquire(&token).await.unwrap();
        let second = gate.acquire(&token).await.unwrap();
        assert!(first._permit.is_none());
        assert!(second._permit.is_none());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_429_feedback_preserves_latest_deadline_and_one_backoff_wave() {
        let gate = RateGate::new(60_000);
        let generation = gate.generation();
        let started = Instant::now();
        let barrier = Arc::new(tokio::sync::Barrier::new(9));
        let mut tasks = Vec::new();
        for seconds in 10..18 {
            let gate = gate.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                gate.throttle(Some(Duration::from_secs(seconds)), generation);
            }));
        }
        barrier.wait().await;
        for task in tasks {
            task.await.unwrap();
        }
        let state = gate.feedback.lock().unwrap();
        assert_eq!(state.consecutive, 1);
        assert_eq!(state.generation, 8);
        assert!(state.until.unwrap() >= started + Duration::from_secs(17));
    }
    #[tokio::test(start_paused = true)]
    async fn concurrent_429s_do_not_multiply_backoff_in_one_generation() {
        let gate = RateGate::new(60_000);
        let generation = gate.generation();
        gate.throttle(None, generation);
        gate.throttle(None, generation);
        let state = gate.feedback.lock().unwrap();
        assert_eq!(state.consecutive, 1);
        assert_eq!(
            state.until.unwrap(),
            Instant::now() + Duration::from_secs(1)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn stale_feedback_does_not_hide_a_newer_throttle_wave() {
        let gate = RateGate::new(60_000);
        let old = gate.generation();
        gate.throttle(None, old);
        let newer = gate.generation();
        gate.throttle(None, old);
        gate.throttle(None, newer);
        let state = gate.feedback.lock().unwrap();
        assert_eq!(state.consecutive, 2);
        assert_eq!(
            state.until.unwrap(),
            Instant::now() + Duration::from_secs(2)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn late_success_does_not_erase_newer_cooldown() {
        let gate = RateGate::new(60_000);
        let token = CancellationToken::new();
        let old = gate.acquire(&token).await.unwrap();
        gate.throttle(None, gate.generation());
        old.complete(200);
        let waiter = tokio::spawn({
            let g = gate.clone();
            let t = token.clone();
            async move { g.acquire(&t).await.unwrap() }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        tokio::time::advance(Duration::from_millis(999)).await;
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(waiter.await.is_ok());
    }
    #[tokio::test(start_paused = true)]
    async fn extended_wait_wakes_probe_and_defers_immediately() {
        let gate = RateGate::new(60_000);
        gate.throttle(Some(Duration::from_secs(10)), gate.generation());
        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.acquire(&CancellationToken::new()).await }
        });
        tokio::task::yield_now().await;
        gate.throttle(Some(Duration::from_secs(601)), gate.generation());
        tokio::task::yield_now().await;
        assert!(waiter.is_finished());
        assert!(waiter.await.unwrap().is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn unrepresentable_provider_wait_never_becomes_a_short_retry() {
        let gate = RateGate::new(60_000);
        gate.throttle(Some(Duration::MAX), gate.generation());
        tokio::time::advance(Duration::from_secs(601)).await;
        assert!(gate.acquire(&CancellationToken::new()).await.is_err());
        assert!(gate.feedback.lock().unwrap().beyond_clock);
    }
    #[tokio::test(start_paused = true)]
    async fn long_provider_wait_defers_without_truncation() {
        let gate = RateGate::new(60_000);
        gate.throttle(Some(Duration::from_secs(601)), gate.generation());
        let result = gate.acquire(&CancellationToken::new()).await;
        assert!(result.is_err());
        assert!(gate.feedback.lock().unwrap().until.unwrap() > Instant::now() + MAX_WAIT);
    }
    #[tokio::test(start_paused = true)]
    async fn cooldown_is_cancellable_and_isolated_per_profile() {
        let first = RateGate::new(60_000);
        let other = RateGate::new(60_000);
        first.throttle(Some(Duration::from_secs(30)), first.generation());
        let cancel = CancellationToken::new();
        assert!(other.acquire(&cancel).await.is_ok());
        let waiting = tokio::spawn({
            let first = first.clone();
            let cancel = cancel.clone();
            async move { first.acquire(&cancel).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        cancel.cancel();
        assert!(waiting.await.unwrap().is_err());
        assert!(
            RateGate::new(60_000)
                .acquire(&CancellationToken::new())
                .await
                .is_ok()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn later_arrival_cannot_bypass_a_queued_waiter_after_probe() {
        let gate = RateGate::new(60_000);
        gate.throttle(Some(Duration::from_secs(1)), gate.generation());
        let token = CancellationToken::new();
        let first = tokio::spawn({
            let g = gate.clone();
            let t = token.clone();
            async move { g.acquire(&t).await.unwrap() }
        });
        tokio::task::yield_now().await;
        let second = tokio::spawn({
            let g = gate.clone();
            let t = token.clone();
            async move { g.acquire(&t).await.unwrap() }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let probe = first.await.unwrap();
        probe.complete(200);
        let later = tokio::spawn({
            let g = gate.clone();
            let t = token.clone();
            async move { g.acquire(&t).await.unwrap() }
        });
        tokio::task::yield_now().await;
        assert!(
            !later.is_finished(),
            "a new request bypassed the queued waiter"
        );
        drop(probe);
        let earlier = second.await.unwrap();
        assert!(
            earlier._permit.is_none(),
            "successful probe clears throttling"
        );
        let later = crate::test_watchdog::watchdog("released FIFO waiter", later)
            .await
            .unwrap()
            .unwrap();
        assert!(
            later._permit.is_none(),
            "released waiters do not serialize HTTP requests"
        );
        drop((earlier, later));
    }
    #[tokio::test(start_paused = true)]
    async fn fifo_and_header_floor_are_respected() {
        let gate = RateGate::new(60_000);
        gate.throttle(Some(Duration::from_secs(3)), gate.generation());
        let token = CancellationToken::new();
        let first = tokio::spawn({
            let gate = gate.clone();
            let token = token.clone();
            async move { gate.acquire(&token).await.unwrap() }
        });
        tokio::task::yield_now().await;
        let second = tokio::spawn({
            let gate = gate.clone();
            let token = token.clone();
            async move { gate.acquire(&token).await.unwrap() }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3)).await;
        let lease = first.await.unwrap();
        assert!(!second.is_finished());
        lease.complete(200);
        drop(lease);
        assert!(second.await.is_ok());
    }
}
