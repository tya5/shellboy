//! The bounded delivery slot for the currently awaited shell call.
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundRelease {
    Background,
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
    pub(super) fn release_matching(&self, execution: Option<&str>) -> Option<String> {
        let current = self.current.lock().expect("foreground slot poisoned");
        let (id, sender) = current.as_ref().filter(|(id, sender)| {
            !sender.is_closed() && execution.is_none_or(|expected| expected == id)
        })?;
        sender.send_if_modified(|pending| {
            if pending.is_some() {
                return false;
            }
            *pending = Some(ForegroundRelease::Background);
            true
        });
        Some(id.clone())
    }

    /// Serialize a short DB update with foreground release, without awaiting.
    pub(super) fn with_current<T>(&self, execution: &str, apply: impl FnOnce() -> T) -> Option<T> {
        let current = self.current.lock().expect("foreground slot poisoned");
        current
            .as_ref()
            .filter(|(id, sender)| id == execution && !sender.is_closed())
            .map(|_| apply())
    }
    pub(super) fn waiting_execution(&self) -> Option<String> {
        self.current
            .lock()
            .expect("foreground slot poisoned")
            .as_ref()
            .filter(|(_, sender)| {
                !sender.is_closed() && *sender.borrow() != Some(ForegroundRelease::Background)
            })
            .map(|(id, _)| id.clone())
    }
    pub(super) fn clear(&self) {
        self.current
            .lock()
            .expect("foreground slot poisoned")
            .take();
    }
}

/// One coalesced release for this exact foreground call.
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
    async fn completion_winning_a_queued_release_does_not_release_the_next_call() {
        let slot = ForegroundSlot::default();
        let mut first = slot.register("fixture");
        assert!(slot.release_matching(Some("fixture")).is_some());
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
        assert!(slot.release_matching(Some("fixture")).is_some());
        assert_eq!(second.recv().await, Some(ForegroundRelease::Background));
        slot.clear();
    }

    #[tokio::test]
    async fn release_after_receiver_completion_is_rejected_and_never_carried_forward() {
        let slot = ForegroundSlot::default();
        let first = slot.register("fixture");
        drop(first);
        assert!(slot.release_matching(Some("fixture")).is_none());
        slot.clear();
        let mut second = slot.register("fixture");
        assert!(!second.0.has_changed().unwrap());
        assert!(slot.release_matching(Some("fixture")).is_some());
        assert_eq!(second.recv().await, Some(ForegroundRelease::Background));
        slot.clear();
    }
}
