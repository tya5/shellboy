use crate::protocol::{ApiError, ErrorCode};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub(super) struct RequestAdmission {
    active_requests: AtomicUsize,
    context_requests: AtomicUsize,
    admission_closed: AtomicBool,
    changed: Arc<Notify>,
}

impl RequestAdmission {
    pub(super) fn new(changed: Arc<Notify>) -> Arc<Self> {
        Arc::new(Self {
            active_requests: AtomicUsize::new(0),
            context_requests: AtomicUsize::new(0),
            admission_closed: AtomicBool::new(false),
            changed,
        })
    }

    pub(super) fn admit(
        self: &Arc<Self>,
        stop: &CancellationToken,
        changes_context: bool,
    ) -> Result<RequestGuard, ApiError> {
        // Close and the context count share a SeqCst order: either this guard is
        // counted before capture, or the second close check rejects admission.
        // Acquire/Release alone on the two atomics permits store buffering.
        if stop.is_cancelled() || self.admission_closed.load(Ordering::SeqCst) {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        self.active_requests.fetch_add(1, Ordering::AcqRel);
        if changes_context {
            self.context_requests.fetch_add(1, Ordering::SeqCst);
        }
        let guard = RequestGuard {
            admission: self.clone(),
            changes_context,
        };
        if stop.is_cancelled() || self.admission_closed.load(Ordering::SeqCst) {
            drop(guard);
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        Ok(guard)
    }

    pub(super) fn close(&self) {
        self.admission_closed.store(true, Ordering::SeqCst);
    }

    pub(super) fn is_closed(&self) -> bool {
        self.admission_closed.load(Ordering::SeqCst)
    }

    pub(super) fn is_idle(&self) -> bool {
        self.active_requests.load(Ordering::SeqCst) == 0
    }

    pub(super) async fn wait_context_zero(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.context_requests.load(Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    pub(super) fn active_count(&self) -> usize {
        self.active_requests.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn context_count(&self) -> usize {
        self.context_requests.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn closed_for_test(&self) -> bool {
        self.admission_closed.load(Ordering::Acquire)
    }
}

pub(super) struct RequestGuard {
    admission: Arc<RequestAdmission>,
    changes_context: bool,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        // A shutdown drain still needs this wake. All four operations are SeqCst: this
        // decrement then the close load here, the close store then the count load in
        // `is_idle`. With weaker orders both loads could read old values (store
        // buffering): the wake skipped and the drain parked for good. In one total order,
        // a load here that misses the close puts the decrement before the drain's load.
        self.admission
            .active_requests
            .fetch_sub(1, Ordering::SeqCst);
        if self.changes_context {
            self.admission
                .context_requests
                .fetch_sub(1, Ordering::SeqCst);
        }
        if self.admission.admission_closed.load(Ordering::SeqCst) {
            self.admission.changed.notify_waiters();
        }
    }
}
