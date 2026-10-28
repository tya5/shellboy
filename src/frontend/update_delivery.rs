//! One bounded, ordered UI outbox shared by the frontend and its finite workers.
use super::*;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
const CRITICAL_LIMIT: usize = 512;
const ADMISSION_LIMIT: usize = 128;
const LATEST_LIMIT: usize = 128;
struct Item {
    sequence: u64,
    value: UiUpdate,
}
#[derive(Default)]
struct Pending {
    sequence: u64,
    critical: VecDeque<Item>,
    latest: BTreeMap<String, Item>,
}
pub(super) trait UpdateTarget: Sync {
    #[allow(clippy::result_large_err)] // Preserve ownership on bounded-channel rejection, without allocation.
    fn try_send(&self, value: UiUpdate) -> Result<(), std::sync::mpsc::TrySendError<UiUpdate>>;
    fn flush_pending(&self) {}
    /// None means buffered/delivered, Some retains ownership for lossless retry.
    fn defer(&self, value: UiUpdate) -> Option<UiUpdate> {
        match self.try_send(value) {
            Ok(()) | Err(std::sync::mpsc::TrySendError::Disconnected(_)) => None,
            Err(std::sync::mpsc::TrySendError::Full(value)) => Some(value),
        }
    }
}
impl UpdateTarget for SyncSender<UiUpdate> {
    #[allow(clippy::result_large_err)] // Preserve ownership on bounded-channel rejection, without allocation.
    fn try_send(&self, value: UiUpdate) -> Result<(), std::sync::mpsc::TrySendError<UiUpdate>> {
        SyncSender::try_send(self, value)
    }
}
struct Delivery {
    raw: SyncSender<UiUpdate>,
    pending: Mutex<Pending>,
    closed: AtomicBool,
    ready: tokio::sync::Notify,
}
#[derive(Clone)]
pub(super) struct BufferedUpdates {
    delivery: Arc<Delivery>,
}
impl BufferedUpdates {
    pub(super) fn new(raw: SyncSender<UiUpdate>) -> Self {
        Self {
            delivery: Arc::new(Delivery {
                raw,
                pending: Mutex::new(Pending::default()),
                closed: AtomicBool::new(false),
                ready: tokio::sync::Notify::new(),
            }),
        }
    }
    /// All producers share the same order and finite outbox; no raw bypass.
    pub(super) fn worker(&self) -> Self {
        self.clone()
    }
    /// Notify keeps one wake permit if a worker enqueues before select polls.
    pub(super) async fn notified(&self) {
        self.delivery.ready.notified().await;
    }
    pub(super) fn backpressured(&self) -> bool {
        self.delivery
            .pending
            .lock()
            .expect("UI delivery lock")
            .critical
            .len()
            >= ADMISSION_LIMIT
    }
    pub(super) fn pending(&self) -> bool {
        let q = self.delivery.pending.lock().expect("UI delivery lock");
        !q.critical.is_empty() || !q.latest.is_empty()
    }
    pub(super) fn closed(&self) -> bool {
        self.delivery.closed.load(Ordering::Relaxed)
    }
    pub(super) async fn finish(
        &self,
        actions: &mut crate::ui::actions::ActionReceiver,
        result: Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        if result.as_ref().err().is_some_and(|error| {
            error
                .details
                .as_ref()
                .is_some_and(|details| details["forced_shutdown"] == true)
        }) {
            return result;
        }
        if self.drain(actions).await {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Forced shutdown while UI delivery was pending",
            )
            .details(serde_json::json!({"forced_shutdown": true})));
        }
        result
    }
    async fn drain(&self, actions: &mut crate::ui::actions::ActionReceiver) -> bool {
        while self.pending() && !self.closed() {
            self.flush();
            if !self.pending() || self.closed() {
                break;
            }
            tokio::select! {
                _=tokio::time::sleep(Duration::from_millis(5))=>{},
                action=actions.recv_with_admission(false)=> {
                    if matches!(action,Some(UiAction::ForceQuit)) {return true;}
                    if action.is_none() {return true;}
                }
            }
        }
        false
    }
    pub(super) fn flush(&self) {
        let mut q = self.delivery.pending.lock().expect("UI delivery lock");
        for _ in 0..32 {
            let oldest = q
                .latest
                .iter()
                .min_by_key(|(_, item)| item.sequence)
                .map(|(key, item)| (key.clone(), item.sequence));
            let key = oldest
                .filter(|(_, sequence)| {
                    q.critical
                        .front()
                        .is_none_or(|front| *sequence < front.sequence)
                })
                .map(|(key, _)| key);
            let item = if let Some(key) = &key {
                q.latest.remove(key)
            } else {
                q.critical.pop_front()
            };
            let Some(mut item) = item else {
                break;
            };
            match self.delivery.raw.try_send(item.value) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(value)) => {
                    item.value = value;
                    if let Some(key) = key {
                        q.latest.insert(key, item);
                    } else {
                        q.critical.push_front(item);
                    }
                    break;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    self.delivery.closed.store(true, Ordering::Relaxed);
                    q.latest.clear();
                    q.critical.clear();
                    break;
                }
            }
        }
    }
}
impl UpdateTarget for BufferedUpdates {
    fn flush_pending(&self) {
        self.flush();
    }
    #[allow(clippy::result_large_err)] // Preserve ownership on bounded-channel rejection, without allocation.
    fn try_send(&self, value: UiUpdate) -> Result<(), std::sync::mpsc::TrySendError<UiUpdate>> {
        use std::sync::mpsc::TrySendError;
        if self.closed() {
            return Err(TrySendError::Disconnected(value));
        }
        let mut q = self.delivery.pending.lock().expect("UI delivery lock");
        let was_empty = q.critical.is_empty() && q.latest.is_empty();
        let value = if was_empty {
            match self.delivery.raw.try_send(value) {
                Ok(()) => return Ok(()),
                Err(std::sync::mpsc::TrySendError::Disconnected(value)) => {
                    self.delivery.closed.store(true, Ordering::Relaxed);
                    return Err(TrySendError::Disconnected(value));
                }
                Err(std::sync::mpsc::TrySendError::Full(value)) => value,
            }
        } else {
            value
        };
        if let Some(key) = latest_key(&value) {
            if !q.latest.contains_key(&key)
                && q.latest.len() >= LATEST_LIMIT
                && let Some(old) = q
                    .latest
                    .iter()
                    .min_by_key(|(_, item)| item.sequence)
                    .map(|(key, _)| key.clone())
            {
                q.latest.remove(&old);
            }
            q.sequence = q.sequence.wrapping_add(1);
            let sequence = q.sequence;
            q.latest.insert(key, Item { sequence, value });
            if was_empty {
                self.delivery.ready.notify_one();
            }
            Ok(())
        } else if q.critical.len() < CRITICAL_LIMIT {
            q.sequence = q.sequence.wrapping_add(1);
            let sequence = q.sequence;
            q.critical.push_back(Item { sequence, value });
            if was_empty {
                self.delivery.ready.notify_one();
            }
            Ok(())
        } else {
            // Reserve exhaustion cannot discard a receipt or report success.
            // The producer retries through this outbox after flushing older items.
            Err(TrySendError::Full(value))
        }
    }
}
fn latest_key(value: &UiUpdate) -> Option<String> {
    Some(match value {
        UiUpdate::Status { thread_id, .. } => format!("status:{thread_id:?}"),
        UiUpdate::DraftStatus { draft_id, .. } => format!("draft:{draft_id}"),
        UiUpdate::Progress { thread_id, .. } => format!("progress:{thread_id}"),
        UiUpdate::ModelReceive {
            thread_id,
            generation,
            ..
        } => format!("receive:{thread_id}:{generation}"),
        UiUpdate::Activity {
            thread_id,
            generation,
            ..
        }
        | UiUpdate::ActivityUnavailable {
            thread_id,
            generation,
            ..
        } => format!("activity:{thread_id}:{generation}"),
        UiUpdate::RecoveryBell => "bell".into(),
        UiUpdate::ShutdownProgress { .. } => "shutdown-progress".into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_status_coalesces_but_ordered_results_and_capacity_are_preserved() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        for n in 0..1000 {
            assert!(
                out.defer(UiUpdate::Status {
                    thread_id: None,
                    text: n.to_string()
                })
                .is_none()
            );
        }
        assert_eq!(out.delivery.pending.lock().unwrap().latest.len(), 1);
        for n in 0..CRITICAL_LIMIT {
            assert!(
                out.defer(UiUpdate::ThreadDeletion {
                    request_id: n as u64,
                    args: ThreadDeleteArgs {
                        thread_id: "t".into(),
                        descendants: false
                    },
                    execute: true,
                    result: Ok(ThreadDeletionReply::Deleted(vec!["t".into()]))
                })
                .is_none()
            );
        }
        assert!(out.backpressured());
        let retained = out.defer(UiUpdate::Shutdown);
        assert!(matches!(retained, Some(UiUpdate::Shutdown)));
        rx.recv().unwrap();
        out.flush();
        assert!(matches!(rx.recv().unwrap(),UiUpdate::Status{text,..} if text=="999"));
        for n in 0..CRITICAL_LIMIT {
            out.flush();
            assert!(
                matches!(rx.recv().unwrap(),UiUpdate::ThreadDeletion{request_id,..}if request_id==n as u64)
            );
        }
        assert!(!out.pending());
    }
    #[test]
    fn ui_disconnect_releases_all_queued_values_and_closes_delivery() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        out.defer(UiUpdate::Shutdown);
        drop(rx);
        out.flush();
        assert!(out.closed());
        assert!(!out.pending());
    }
    #[tokio::test]
    async fn worker_completion_cannot_be_followed_by_older_preparing_status() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        update(
            &out,
            UiUpdate::Status {
                thread_id: None,
                text: "preparing".into(),
            },
        )
        .await;
        let worker = out.worker();
        let task = tokio::spawn(async move {
            update(
                &worker,
                UiUpdate::Status {
                    thread_id: None,
                    text: "completed".into(),
                },
            )
            .await;
            update(&worker, UiUpdate::Shutdown).await;
        });
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        rx.recv().unwrap();
        let mut statuses = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                out.flush();
                while let Ok(value) = rx.try_recv() {
                    match value {
                        UiUpdate::Status { text, .. } => statuses.push(text),
                        UiUpdate::Shutdown => return,
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(statuses.last().map(String::as_str), Some("completed"));
        assert!(!out.pending());
    }

    #[tokio::test]
    async fn full_outbox_retry_never_overtakes_older_critical_results() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        for n in 0..CRITICAL_LIMIT {
            assert!(
                out.defer(UiUpdate::ThreadDeletion {
                    request_id: n as u64,
                    args: ThreadDeleteArgs {
                        thread_id: "t".into(),
                        descendants: false
                    },
                    execute: true,
                    result: Ok(ThreadDeletionReply::Deleted(vec!["t".into()])),
                })
                .is_none()
            );
        }
        let worker = out.worker();
        let task = tokio::spawn(async move {
            update(&worker, UiUpdate::Shutdown).await;
        });
        tokio::task::yield_now().await;
        rx.recv().unwrap();
        let mut count = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                out.flush();
                while let Ok(value) = rx.try_recv() {
                    match value {
                        UiUpdate::ThreadDeletion { request_id, .. } => {
                            assert_eq!(request_id, count);
                            count += 1;
                        }
                        UiUpdate::Shutdown => {
                            assert_eq!(count, CRITICAL_LIMIT as u64);
                            return;
                        }
                        _ => panic!("unexpected UI update"),
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn final_ui_drain_honors_force_without_starting_another_grace() {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        out.defer(UiUpdate::Shutdown);
        let (actions, mut receiver) = crate::ui::actions::channel(1);
        actions.try_send(UiAction::ForceQuit).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(250), out.drain(&mut receiver))
                .await
                .unwrap()
        );
        assert!(out.pending()); // An explicit forced exit may abandon undelivered UI values.
    }
    #[tokio::test]
    async fn accepted_force_at_full_reserve_never_waits_for_ui_or_second_force() {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        for n in 0..CRITICAL_LIMIT {
            assert!(
                out.defer(UiUpdate::ThreadDeletion {
                    request_id: n as u64,
                    args: ThreadDeleteArgs {
                        thread_id: "t".into(),
                        descendants: false
                    },
                    execute: true,
                    result: Ok(ThreadDeletionReply::Deleted(vec!["t".into()])),
                })
                .is_none()
            );
        }
        let (actions, mut receiver) = crate::ui::actions::channel(1);
        let worker = out.worker();
        let task = tokio::spawn(async move {
            let result = wait_for_shutdown_updates(
                &mut receiver,
                &worker,
                Duration::ZERO,
                std::future::pending(),
            )
            .await;
            worker.finish(&mut receiver, result).await
        });
        // The first tick enables force. Observe it through the bounded shared
        // outbox without freeing any raw UI capacity or sending a second force.
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if out
                    .delivery
                    .pending
                    .lock()
                    .unwrap()
                    .latest
                    .values()
                    .any(|item| {
                        matches!(
                            item.value,
                            UiUpdate::ShutdownProgress {
                                force_available: true,
                                ..
                            }
                        )
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        actions.try_send(UiAction::ForceQuit).unwrap();
        let error = tokio::time::timeout(Duration::from_millis(250), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.details.unwrap()["forced_shutdown"], true);
        assert_eq!(
            out.delivery.pending.lock().unwrap().critical.len(),
            CRITICAL_LIMIT
        );
    }
    #[tokio::test]
    async fn worker_enqueue_wakes_an_idle_select_with_flush_initially_disabled() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        let worker = out.worker();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let bridge = tokio::spawn(async move {
            let pending = out.pending();
            assert!(!pending);
            entered.send(()).unwrap();
            tokio::select! {
                _ = out.notified() => out.flush(),
                _ = tokio::time::sleep(Duration::from_secs(30)), if pending => out.flush(),
            }
        });
        waiting.await.unwrap();
        // The raw queue is still full, so the worker must use the empty outbox.
        update(&worker, UiUpdate::Shutdown).await;
        assert!(worker.pending());
        assert!(matches!(rx.try_recv().unwrap(), UiUpdate::RecoveryBell));
        tokio::time::timeout(Duration::from_secs(1), bridge)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(rx.try_recv().unwrap(), UiUpdate::Shutdown));
        assert!(!worker.pending());
    }
}
