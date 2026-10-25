//! The bounded delivery slot for the currently awaited shell call.
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForegroundRelease {
    Background,
    AdditionalInput,
}

/// One bounded delivery slot for the currently awaited call; no permit survives
/// its receiver. Run nudges and durable input remain independent of this slot.
#[derive(Default)]
pub(super) struct ForegroundSlot {
    current: std::sync::Mutex<Option<(String, watch::Sender<Option<ForegroundRelease>>)>>,
}
impl ForegroundSlot {
    pub(super) fn register(&self, execution: &str) -> ForegroundReceiver {
        let (sender, receiver) = watch::channel(None);
        *self.current.lock().expect("foreground slot poisoned") = Some((execution.into(), sender));
        ForegroundReceiver(receiver)
    }
    pub(super) fn release(&self, reason: ForegroundRelease) -> bool {
        let current = self.current.lock().expect("foreground slot poisoned");
        let Some((_, sender)) = current.as_ref().filter(|(_, sender)| !sender.is_closed()) else {
            return false;
        };
        sender.send_if_modified(|pending| {
            if *pending == Some(ForegroundRelease::Background) {
                // A human action is sticky until this call ends. A stale input
                // nudge must neither reject nor replace it.
                false
            } else {
                *pending = Some(reason);
                // Re-notify even for another input wake: the earlier one may
                // already have been consumed before a new receipt committed.
                true
            }
        });
        true
    }

    /// Serialize a short DB update with foreground release, without awaiting.
    pub(super) fn with_current<T>(&self, execution: &str, apply: impl FnOnce() -> T) -> Option<T> {
        let current = self.current.lock().expect("foreground slot poisoned");
        current
            .as_ref()
            .filter(|(id, sender)| id == execution && !sender.is_closed())
            .map(|_| apply())
    }
    pub(super) fn clear(&self) {
        self.current
            .lock()
            .expect("foreground slot poisoned")
            .take();
    }
}

/// One coalesced reason for this call, rather than a queue of stale input wakes.
pub(super) struct ForegroundReceiver(watch::Receiver<Option<ForegroundRelease>>);
impl ForegroundReceiver {
    pub(super) async fn recv(&mut self) -> Option<ForegroundRelease> {
        self.0.changed().await.ok()?;
        *self.0.borrow_and_update()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn human_background_is_not_rejected_by_queued_additional_input() {
        let slot = ForegroundSlot::default();
        let mut call = slot.register("current");
        assert!(slot.release(ForegroundRelease::AdditionalInput));
        assert!(
            slot.release(ForegroundRelease::Background),
            "human control must not be rejected by an obsolete input wake"
        );
        assert!(slot.release(ForegroundRelease::AdditionalInput));
        assert_eq!(call.recv().await, Some(ForegroundRelease::Background));
        slot.clear();
    }

    #[tokio::test]
    async fn consumed_input_wake_does_not_hide_a_later_input() {
        let slot = ForegroundSlot::default();
        let mut call = slot.register("current");
        for _ in 0..2 {
            assert!(slot.release(ForegroundRelease::AdditionalInput));
            assert_eq!(call.recv().await, Some(ForegroundRelease::AdditionalInput));
        }
        slot.clear();
    }

    #[tokio::test]
    async fn completion_winning_a_queued_release_does_not_release_the_next_call() {
        for reason in [
            ForegroundRelease::Background,
            ForegroundRelease::AdditionalInput,
        ] {
            let slot = ForegroundSlot::default();
            let mut first = slot.register("fixture");
            assert!(slot.release(reason));
            let (completed, completion) = tokio::sync::oneshot::channel();
            completed.send(()).unwrap();
            // Both completion and the user action are ready. Exercise the exact
            // ordering that used to leave a Run-wide Notify permit behind.
            tokio::select! {
                biased;
                _ = completion => {},
                _ = first.recv() => panic!("fixture must choose completion"),
            }
            slot.clear();
            drop(first);
            let mut second = slot.register("fixture");
            assert!(!second.0.has_changed().unwrap());
            assert!(slot.release(ForegroundRelease::Background));
            assert_eq!(second.recv().await, Some(ForegroundRelease::Background));
            slot.clear();
        }
    }

    #[tokio::test]
    async fn release_after_receiver_completion_is_rejected_and_never_carried_forward() {
        let slot = ForegroundSlot::default();
        let first = slot.register("fixture");
        drop(first);
        assert!(!slot.release(ForegroundRelease::Background));
        slot.clear();
        let mut second = slot.register("fixture");
        assert!(!second.0.has_changed().unwrap());
        assert!(slot.release(ForegroundRelease::AdditionalInput));
        assert_eq!(
            second.recv().await,
            Some(ForegroundRelease::AdditionalInput)
        );
        slot.clear();
    }
}
