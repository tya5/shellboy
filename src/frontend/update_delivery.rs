//! The frontend task owns deferred UI delivery; worker senders retain backpressure.
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
    fn raw(&self) -> &SyncSender<UiUpdate>;
    /// None means buffered/delivered, Some retains ownership for lossless retry.
    fn defer(&self, value: UiUpdate) -> Option<UiUpdate> {
        Some(value)
    }
}
impl UpdateTarget for SyncSender<UiUpdate> {
    fn raw(&self) -> &SyncSender<UiUpdate> {
        self
    }
}
pub(super) struct BufferedUpdates {
    raw: SyncSender<UiUpdate>,
    pending: Mutex<Pending>,
    closed: AtomicBool,
}
impl BufferedUpdates {
    pub(super) fn new(raw: SyncSender<UiUpdate>) -> Self {
        Self {
            raw,
            pending: Mutex::new(Pending::default()),
            closed: AtomicBool::new(false),
        }
    }
    /// Spawned workers explicitly use the existing bounded raw queue, holding
    /// their existing permit while waiting. No worker inherits the outbox.
    pub(super) fn worker(&self) -> SyncSender<UiUpdate> {
        self.raw.clone()
    }
    pub(super) fn backpressured(&self) -> bool {
        self.pending
            .lock()
            .expect("UI delivery lock")
            .critical
            .len()
            >= ADMISSION_LIMIT
    }
    pub(super) fn pending(&self) -> bool {
        let q = self.pending.lock().expect("UI delivery lock");
        !q.critical.is_empty() || !q.latest.is_empty()
    }
    pub(super) fn closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
    pub(super) async fn finish(
        &self,
        actions: &mut crate::ui::actions::ActionReceiver,
        grace: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        while self.pending() && !self.closed() {
            self.flush();
            if !self.pending() || self.closed() {
                break;
            }
            tokio::select! {
                _=tokio::time::sleep(Duration::from_millis(5))=>{},
                action=actions.recv_with_admission(false)=> {
                    if matches!(action,Some(UiAction::ForceQuit))&&tokio::time::Instant::now()>=deadline {return true;}
                    if action.is_none() {return true;}
                }
            }
        }
        false
    }
    pub(super) fn flush(&self) {
        let mut q = self.pending.lock().expect("UI delivery lock");
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
            match self.raw.try_send(item.value) {
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
                    self.closed.store(true, Ordering::Relaxed);
                    q.latest.clear();
                    q.critical.clear();
                    break;
                }
            }
        }
    }
}
impl UpdateTarget for BufferedUpdates {
    fn raw(&self) -> &SyncSender<UiUpdate> {
        &self.raw
    }
    fn defer(&self, value: UiUpdate) -> Option<UiUpdate> {
        if self.closed() {
            return None;
        }
        let mut q = self.pending.lock().expect("UI delivery lock");
        let value = if q.critical.is_empty() && q.latest.is_empty() {
            match self.raw.try_send(value) {
                Ok(()) => return None,
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    self.closed.store(true, Ordering::Relaxed);
                    return None;
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
            None
        } else if q.critical.len() < CRITICAL_LIMIT {
            q.sequence = q.sequence.wrapping_add(1);
            let sequence = q.sequence;
            q.critical.push_back(Item { sequence, value });
            None
        } else {
            // Reserve exhaustion cannot discard a receipt or report success.
            // Keep the original value for raw queue backpressure as a last resort.
            Some(value)
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
        assert_eq!(out.pending.lock().unwrap().latest.len(), 1);
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
}
