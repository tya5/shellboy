//! One UI owner's ephemeral selected-Thread subscription, separate from observers.
use super::*;
use std::sync::Mutex as StdMutex;
pub(super) struct UiEvents {
    pub(super) sender: broadcast::Sender<EngineEvent>,
    owner: StdMutex<(u64, Option<String>)>,
}
impl UiEvents {
    pub(super) fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            sender,
            owner: StdMutex::new((0, None)),
        }
    }
    fn interested(&self, event: &EngineEvent) -> bool {
        match event {
            EngineEvent::Stream { thread_id, .. } | EngineEvent::ModelReceive { thread_id, .. } => {
                self.owner.lock().expect("UI subscription lock").1.as_ref() == Some(thread_id)
            }
            _ => true,
        }
    }
}
pub(crate) struct UiSubscription {
    control: Arc<Control>,
    owner: u64,
    pub(crate) receiver: broadcast::Receiver<EngineEvent>,
}
impl UiSubscription {
    pub(crate) fn belongs_to(&self, owner: u64) -> bool {
        self.owner == owner
    }
    pub(crate) fn select(&self, thread: Option<&str>) {
        let mut selected = self
            .control
            .ui_events
            .owner
            .lock()
            .expect("UI subscription lock");
        if selected.0 == self.owner && selected.1.as_deref() != thread {
            selected.1 = thread.map(str::to_owned);
        }
    }
}
impl Drop for UiSubscription {
    fn drop(&mut self) {
        self.select(None);
    }
}
impl Control {
    pub(super) async fn memory_ui_snapshot(
        &self,
        disconnected: CancellationToken,
    ) -> Option<Value> {
        if disconnected.is_cancelled()
            || self.stop.is_cancelled()
            || self.ui_events.sender.receiver_count() == 0
        {
            return None;
        }
        let owner = self.ui_events.owner.lock().expect("UI subscription lock").0;
        let (reply, receive) = crate::memory::UiMemoryReply::new();
        self.ui_events
            .sender
            .send(EngineEvent::MemoryProbe { owner, reply })
            .ok()?;
        tokio::select! { biased;
            _ = self.stop.cancelled() => None,
            _ = disconnected.cancelled() => None,
            result = receive => result.ok(),
        }
    }
    pub(crate) fn subscribe_ui(self: &Arc<Self>) -> UiSubscription {
        let mut selected = self.ui_events.owner.lock().expect("UI subscription lock");
        selected.0 = selected.0.wrapping_add(1);
        selected.1 = None;
        UiSubscription {
            control: self.clone(),
            owner: selected.0,
            receiver: self.ui_events.sender.subscribe(),
        }
    }
    pub(super) fn publish_event(&self, event: EngineEvent) {
        if self.ui_events.interested(&event) {
            let _ = self.ui_events.sender.send(event.clone());
        }
        let _ = self.events.send(event);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stale_guard_cannot_clear_new_owner_and_observers_keep_every_offset() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let old = control.subscribe_ui();
        old.select(Some("old"));
        let mut current = control.subscribe_ui();
        current.select(Some("selected"));
        drop(old);
        let mut observer = control.subscribe();
        for (thread, offset) in [("hidden", 0), ("selected", 5)] {
            control.publish_event(EngineEvent::Stream {
                thread_id: thread.into(),
                node_id: "attempt".into(),
                text: "delta".into(),
                reasoning: false,
                stream_char_offset: offset,
            });
        }
        assert!(matches!(
            current.receiver.try_recv(),
            Ok(EngineEvent::Stream {
                stream_char_offset: 5,
                ..
            })
        ));
        assert!(current.receiver.try_recv().is_err());
        assert!(matches!(
            observer.try_recv(),
            Ok(EngineEvent::Stream {
                stream_char_offset: 0,
                ..
            })
        ));
        assert!(matches!(
            observer.try_recv(),
            Ok(EngineEvent::Stream {
                stream_char_offset: 5,
                ..
            })
        ));
        current.select(None);
        control.publish_event(EngineEvent::Changed {
            thread_id: "hidden".into(),
            recovery: false,
        });
        assert!(
            matches!(current.receiver.try_recv(),Ok(EngineEvent::Changed{thread_id,..})if thread_id=="hidden")
        );
        drop(current);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
