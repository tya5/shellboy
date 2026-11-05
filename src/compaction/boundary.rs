//! Temporary release of an existing Thread boundary while admission waits.
use std::future::Future;
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) enum PauseError {
    Cancelled,
}

pub(crate) struct Boundary<'a> {
    mutex: &'a Mutex<()>,
    guard: Option<MutexGuard<'a, ()>>,
}
impl<'a> Boundary<'a> {
    pub(crate) fn new(mutex: &'a Mutex<()>, guard: MutexGuard<'a, ()>) -> Self {
        Self {
            mutex,
            guard: Some(guard),
        }
    }
    pub(crate) async fn pause<T>(
        &mut self,
        cancel: &CancellationToken,
        wait: impl Future<Output = T>,
    ) -> Result<T, PauseError> {
        drop(self.guard.take());
        let outcome = tokio::select! { biased; _ = cancel.cancelled() => return Err(PauseError::Cancelled), outcome = wait => outcome };
        let guard = tokio::select! { biased; _ = cancel.cancelled() => return Err(PauseError::Cancelled), guard = self.mutex.lock() => guard };
        self.guard = Some(guard);
        Ok(outcome)
    }
    pub(crate) fn is_held(&self) -> bool {
        self.guard.is_some()
    }
    pub(crate) fn into_guard(self) -> Option<MutexGuard<'a, ()>> {
        self.guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn pause_releases_boundary_then_reacquires_before_return() {
        let mutex = Arc::new(Mutex::new(()));
        let (ready_tx, ready_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn({
            let mutex = mutex.clone();
            async move {
                let guard = mutex.lock().await;
                let mut boundary = Boundary::new(&mutex, guard);
                let result = boundary
                    .pause(&CancellationToken::new(), async {
                        ready_tx.send(()).unwrap();
                        release_rx.await.unwrap();
                        42
                    })
                    .await
                    .unwrap();
                assert_eq!(result, 42);
                assert!(boundary.into_guard().is_some());
            }
        });
        ready_rx.await.unwrap();
        let other = mutex
            .try_lock()
            .expect("admission wait must release boundary");
        release_tx.send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "boundary must be reacquired first");
        drop(other);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn cancellation_during_reacquire_does_not_wait_for_the_boundary() {
        let mutex = Arc::new(Mutex::new(()));
        let cancel = CancellationToken::new();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn({
            let mutex = mutex.clone();
            let cancel = cancel.clone();
            async move {
                let guard = mutex.lock().await;
                let mut boundary = Boundary::new(&mutex, guard);
                let result = boundary
                    .pause(&cancel, async {
                        ready_tx.send(()).unwrap();
                        release_rx.await.unwrap();
                    })
                    .await;
                assert!(result.is_err());
                assert!(boundary.into_guard().is_none());
            }
        });
        ready_rx.await.unwrap();
        let held = mutex.lock().await;
        release_tx.send(()).unwrap();
        tokio::task::yield_now().await;
        cancel.cancel();
        crate::test_watchdog::watchdog("cancel boundary reacquisition", task)
            .await
            .unwrap()
            .unwrap();
        drop(held);
    }
    #[tokio::test]
    async fn cancelled_pause_leaves_boundary_unheld() {
        let mutex = Mutex::new(());
        let mut boundary = Boundary::new(&mutex, mutex.lock().await);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            boundary
                .pause(&cancel, std::future::pending::<()>())
                .await
                .is_err()
        );
        assert!(boundary.into_guard().is_none());
        assert!(mutex.try_lock().is_ok());
    }
}
