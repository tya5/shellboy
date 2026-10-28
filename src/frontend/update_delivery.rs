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
const MAIN_RESERVE: usize = 32;
const WORKER_LIMIT: usize = CRITICAL_LIMIT - MAIN_RESERVE;
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
pub(super) struct BufferedUpdates {
    delivery: Arc<Delivery>,
    worker: bool,
}
// Cloning may cross a task boundary: it must never duplicate main reserve rights.
impl Clone for BufferedUpdates {
    fn clone(&self) -> Self {
        self.worker()
    }
}
impl BufferedUpdates {
    pub(super) fn new(raw: SyncSender<UiUpdate>) -> Self {
        Self {
            worker: false,
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
        Self {
            delivery: self.delivery.clone(),
            worker: true,
        }
    }
    /// A main-loop handler emits at most two critical updates. Workers cannot
    /// consume the reserve between this check and those nonblocking insertions.
    pub(super) fn main_ready(&self) -> bool {
        self.delivery
            .pending
            .lock()
            .expect("UI delivery lock")
            .critical
            .len()
            <= WORKER_LIMIT
    }
    pub(super) fn main_slot(&self) -> bool {
        self.delivery
            .pending
            .lock()
            .expect("UI delivery lock")
            .critical
            .len()
            < CRITICAL_LIMIT
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
        } else if q.critical.len()
            < if self.worker {
                WORKER_LIMIT
            } else {
                CRITICAL_LIMIT
            }
        {
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
    fn critical(request_id: u64) -> UiUpdate {
        UiUpdate::ThreadDeletion {
            request_id,
            args: ThreadDeleteArgs {
                thread_id: "t".into(),
                descendants: false,
            },
            execute: true,
            result: Ok(ThreadDeletionReply::Deleted(vec!["t".into()])),
        }
    }
    fn receipt_startup() -> receipt_delivery::Startup {
        let mut pending = HashMap::new();
        let mut receipts = Vec::new();
        for revision in 0..receipt_delivery::BATCH_LIMIT as u64 {
            let message = format!("message-{revision}");
            pending.insert(
                message.clone(),
                receipt_delivery::PendingReceipt {
                    thread: "thread".into(),
                    draft: "draft".into(),
                    revision,
                    checked: true,
                    repeat: false,
                },
            );
            receipts.push((message, ReceiptStatus::Completed));
        }
        receipt_delivery::Startup {
            pending,
            batch: receipt_delivery::Batch::new(receipts),
        }
    }
    #[test]
    fn cloned_main_handle_cannot_consume_the_main_reserve() {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let main = BufferedUpdates::new(tx);
        let cloned = main.clone();
        for n in 0..WORKER_LIMIT {
            assert!(cloned.defer(critical(n as u64)).is_none());
        }
        assert!(cloned.defer(critical(9999)).is_some());
        for n in WORKER_LIMIT..CRITICAL_LIMIT {
            assert!(main.defer(critical(n as u64)).is_none());
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_frontend_cancel_quit_and_one_force_survive_full_reserve_and_retained_batch() {
        let (root, owner, control, server) = control_tests::fixture().await;
        // Isolate delivery pressure from same-Thread selection-revision CAS:
        // each queued request has a distinct existing recovery-wait owner and
        // stays held, so this fixture starts no model HTTP request.
        owner.db.call(|db| {
            let tx = db.transaction()?;
            for n in 0..40 {
                let thread = format!("closing-thread-{n}");
                let run = format!("closing-run-{n}");
                tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) SELECT ?1,?1,cwd,agents_path,profile_id,model_id,'now' FROM thread WHERE id='thread'", [&thread])?;
                tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?2,'live','now')", rusqlite::params![run,thread])?;
                tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES(?1,?2,?1,'recovery_wait','now')", rusqlite::params![run,thread])?;
            }
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        let worker = out.worker();
        for n in 0..WORKER_LIMIT {
            assert!(worker.defer(critical(n as u64)).is_none());
        }
        assert!(worker.defer(critical(WORKER_LIMIT as u64)).is_some());
        for n in WORKER_LIMIT..CRITICAL_LIMIT {
            assert!(out.defer(critical(n as u64)).is_none());
        }
        let (sender, receiver) = crate::ui::actions::channel(64);
        sender
            .try_send(UiAction::RenderWidth { width: 120 })
            .unwrap();
        let closing_cwd = root.path().to_path_buf();
        let observer = out.worker();
        let bridge = tokio::spawn(run_with_delivery(
            control.clone(),
            receiver,
            out,
            root.path().into(),
            owner,
            server,
            Arc::new(ClipboardAdapter::new().unwrap()),
            Arc::new(Semaphore::new(32)),
            Some(receipt_startup()),
        ));
        sender
            .try_send(UiAction::Cancel {
                thread_id: "thread".into(),
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state: String = control
                    .store
                    .database
                    .call(|db| {
                        Ok(db.query_row(
                            "SELECT state FROM run WHERE id='target-run'",
                            [],
                            |row| row.get(0),
                        )?)
                    })
                    .await
                    .unwrap();
                if state == "stopped" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // No UI receiver has read a single update. Cancellation is real durable
        // control work, not just acceptance into the reserved action mailbox.
        assert_eq!(
            observer.delivery.pending.lock().unwrap().critical.len(),
            CRITICAL_LIMIT
        );
        // Closing sends persist before any UI result wait. More sends than
        // reserved slots must still reach Quit without consuming UI capacity.
        for revision in 0..40 {
            sender
                .try_send(UiAction::Send {
                    draft_id: format!("closing-{revision}"),
                    revision,
                    thread_id: Some(format!("closing-thread-{revision}")),
                    cwd: closing_cwd.clone(),
                    text: "synthetic queued closing request".into(),
                    attachments: vec![],
                    connection: None,
                    model: None,
                    effort: None,
                })
                .unwrap();
        }
        sender
            .try_send(UiAction::SaveDraft {
                thread_id: "thread".into(),
                draft: DraftSnapshot {
                    draft_id: "closing-draft".into(),
                    text: "saved with UI stopped".into(),
                    revision: 99,
                    ..Default::default()
                },
                anchor: ViewAnchor::default(),
                follow: true,
            })
            .unwrap();
        sender.try_send(UiAction::Quit).unwrap();
        // Observe durable effects independently of the owner DB worker, which
        // normal shutdown is allowed to close before presentation resumes.
        let db = rusqlite::Connection::open_with_flags(
            root.path().join("data/shellboy.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let saved: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM draft WHERE id='closing-draft' AND revision=99)", [], |r| r.get(0)).unwrap();
                let sent: i64 = db.query_row("SELECT count(*) FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE n.kind='user' AND r.target_thread_id GLOB 'closing-thread-*' AND n.payload_json LIKE '%synthetic queued closing request%'", [], |r| r.get(0)).unwrap();
                if saved && sent == 40 { break; }
                assert!(!bridge.is_finished(), "shutdown must retain pending UI results");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap_or_else(|error| {
            let draft: Option<i64> = db.query_row("SELECT max(revision) FROM draft WHERE id='closing-draft'", [], |r| r.get(0)).unwrap();
            let mut statement = db.prepare("SELECT kind,count(*) FROM node WHERE payload_json LIKE '%synthetic queued closing request%' GROUP BY kind ORDER BY kind").unwrap();
            let nodes = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            let mut statement = db.prepare("SELECT r.state,count(*) FROM receipt r JOIN node n ON n.id=r.message_node_id WHERE n.kind='user' AND n.payload_json LIKE '%synthetic queued closing request%' GROUP BY r.state ORDER BY r.state").unwrap();
            let receipts = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            panic!("durable closing work timed out: {error}; draft_revision={draft:?}, nodes_by_kind={nodes:?}, receipts_by_state={receipts:?}, frontend_finished={}", bridge.is_finished());
        });
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if observer
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
                assert!(
                    !bridge.is_finished(),
                    "ordinary Quit cannot discard retained results"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        sender.try_send(UiAction::ForceQuit).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), bridge)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.details.unwrap()["forced_shutdown"], true);
    }
    #[tokio::test]
    async fn completed_owned_shutdown_honors_one_force_while_terminal_marker_is_retained() {
        for failed in [false, true] {
            let (tx, _rx) = std::sync::mpsc::sync_channel(1);
            tx.send(UiUpdate::RecoveryBell).unwrap();
            let out = BufferedUpdates::new(tx);
            for n in 0..CRITICAL_LIMIT {
                assert!(out.defer(critical(n as u64)).is_none());
            }
            let (sender, mut actions) = crate::ui::actions::channel(1);
            sender.try_send(UiAction::ForceQuit).unwrap();
            let result = if failed {
                Err(ApiError::new(
                    ErrorCode::IoFailed,
                    "synthetic shutdown error",
                ))
            } else {
                Ok(())
            };
            // Owned work is already done; final presentation cannot restart its
            // grace or consume this explicit force without acting on it.
            let error = tokio::time::timeout(
                Duration::from_millis(250),
                wait_for_shutdown_delivery(&mut actions, &out, Duration::from_secs(60), async {
                    result
                }),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(error.details.unwrap()["forced_shutdown"], true);
            assert_eq!(
                out.delivery.pending.lock().unwrap().critical.len(),
                CRITICAL_LIMIT
            );
        }
    }
    #[tokio::test]
    async fn completed_shutdown_terminal_marker_remains_after_all_older_results() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        for n in 0..CRITICAL_LIMIT {
            assert!(out.defer(critical(n as u64)).is_none());
        }
        let (_sender, mut actions) = crate::ui::actions::channel(1);
        let producer = out.clone();
        let task = tokio::spawn(async move {
            wait_for_shutdown_delivery(&mut actions, &producer, Duration::from_secs(60), async {
                Ok(())
            })
            .await
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        assert!(matches!(rx.try_recv().unwrap(), UiUpdate::RecoveryBell));
        let mut count = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
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
                        _ => panic!("unexpected terminal update"),
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        task.await.unwrap().unwrap();
        assert!(!out.pending());
    }
    #[tokio::test]
    async fn reserved_batch_continuation_keeps_fifo_and_worker_permit_until_ui_resumes() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        let worker = out.worker();
        for n in 0..WORKER_LIMIT {
            assert!(worker.defer(critical(n as u64)).is_none());
        }
        let receipt_delivery::Startup {
            mut pending,
            mut batch,
        } = receipt_startup();
        batch.drain(&mut pending, &out);
        assert_eq!(
            out.delivery.pending.lock().unwrap().critical.len(),
            CRITICAL_LIMIT
        );
        assert!(batch.active());
        let slots = Arc::new(Semaphore::new(1));
        let permit = slots.clone().acquire_owned().await.unwrap();
        let task = tokio::spawn(async move {
            let _permit = permit;
            update(&worker, critical(9999)).await;
        });
        tokio::task::yield_now().await;
        assert_eq!(slots.available_permits(), 0);
        for _ in 0..10 {
            batch.drain(&mut pending, &out);
        }
        assert_eq!(pending.len(), receipt_delivery::BATCH_LIMIT - MAIN_RESERVE);
        rx.try_recv().unwrap();
        let mut older = 0;
        let mut revisions = Vec::new();
        let mut worker_finished = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                batch.drain(&mut pending, &out);
                out.flush();
                while let Ok(value) = rx.try_recv() {
                    match value {
                        UiUpdate::ThreadDeletion {
                            request_id: 9999, ..
                        } => worker_finished = true,
                        UiUpdate::ThreadDeletion { request_id, .. } => {
                            assert_eq!(request_id, older);
                            older += 1;
                        }
                        UiUpdate::Receipt { revision, .. } => {
                            assert_eq!(older, WORKER_LIMIT as u64);
                            revisions.push(revision);
                        }
                        _ => panic!("unexpected result"),
                    }
                }
                if !batch.active() && !out.pending() && worker_finished {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(slots.available_permits(), 1);
        assert!(pending.is_empty());
        assert_eq!(
            revisions,
            (0..receipt_delivery::BATCH_LIMIT as u64).collect::<Vec<_>>()
        );
    }
    #[test]
    fn disconnect_releases_the_bounded_batch_and_outbox() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let out = BufferedUpdates::new(tx);
        for n in 0..CRITICAL_LIMIT {
            assert!(out.defer(critical(n as u64)).is_none());
        }
        let receipt_delivery::Startup {
            mut pending,
            mut batch,
        } = receipt_startup();
        drop(rx);
        out.flush();
        batch.drain(&mut pending, &out);
        assert!(out.closed());
        assert!(!out.pending());
        assert!(!batch.active());
    }
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
        let producer = out.clone();
        let task = tokio::spawn(async move {
            update(&producer, UiUpdate::Shutdown).await;
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
    #[tokio::test]
    async fn shutdown_progress_wakes_delivery_and_enables_one_force_after_ui_backpressure() {
        let (tx, rx) = std::sync::mpsc::sync_channel(128);
        for _ in 0..128 {
            tx.send(UiUpdate::RecoveryBell).unwrap();
        }
        let out = BufferedUpdates::new(tx);
        let worker = out.worker();
        let (sender, mut actions) = crate::ui::actions::channel(1);
        let task = tokio::spawn(async move {
            wait_for_shutdown_delivery(
                &mut actions,
                &worker,
                Duration::from_millis(20),
                std::future::pending(),
            )
            .await
        });
        // Start with no pending outbox so the timer branch is initially disabled.
        // Actual shutdown progress is the producer; the fixture never flushes it.
        tokio::time::timeout(Duration::from_secs(2), async {
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
        for _ in 0..128 {
            assert!(matches!(rx.try_recv().unwrap(), UiUpdate::RecoveryBell));
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(UiUpdate::ShutdownProgress {
                    force_available: true,
                    ..
                }) = rx.try_recv()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        // UI has now received the real force-available state; one request suffices.
        sender.try_send(UiAction::ForceQuit).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.details.unwrap()["forced_shutdown"], true);
    }
}
