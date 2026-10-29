//! Existing autosave queue, per-Thread coalescing and worker lifetime.
use super::*;
type Snapshot = (String, DraftSnapshot, ViewAnchor, bool);
struct PendingSave {
    draft: DraftSnapshot,
    anchor: ViewAnchor,
    follow: bool,
    notified: bool,
}
pub(super) struct DraftSaver {
    backlog: HashMap<String, PendingSave>,
    sender: mpsc::Sender<Snapshot>,
    worker: tokio::task::JoinHandle<Result<(), ApiError>>,
    // Stop pumping a dead worker; each retained Thread is warned once.
    closed: bool,
}
impl DraftSaver {
    pub(super) fn new(control: Arc<Control>, results: mpsc::Sender<BackgroundResult>) -> Self {
        let (sender, mut receiver) = mpsc::channel::<Snapshot>(32);
        let worker = tokio::spawn(async move {
            let mut failure = None;
            while let Some((thread, draft, anchor, follow)) = receiver.recv().await {
                let result = save_draft(&control, thread.clone(), draft, anchor, follow).await;
                if let Err(error) = &result {
                    failure = Some(error.clone());
                }
                let _ = results
                    .send(BackgroundResult::DraftSaved {
                        thread_id: thread,
                        result,
                    })
                    .await;
            }
            failure.map_or(Ok(()), Err)
        });
        Self {
            backlog: HashMap::new(),
            sender,
            worker,
            closed: false,
        }
    }
    pub(super) fn enqueue(
        &mut self,
        thread: String,
        draft: DraftSnapshot,
        anchor: ViewAnchor,
        follow: bool,
        closing: bool,
    ) -> bool {
        if closing || self.backlog.len() < 64 || self.backlog.contains_key(&thread) {
            let notified = self.backlog.get(&thread).is_some_and(|save| save.notified);
            self.backlog.insert(
                thread,
                PendingSave {
                    draft,
                    anchor,
                    follow,
                    notified,
                },
            );
            true
        } else {
            false
        }
    }
    pub(super) fn pump(&mut self) -> Option<String> {
        if self.closed {
            return self.backlog.iter_mut().find_map(|(thread, save)| {
                if save.notified {
                    return None;
                }
                save.notified = true;
                Some(thread.clone())
            });
        }
        while let Some(thread) = self.backlog.keys().next().cloned() {
            let PendingSave {
                draft,
                anchor,
                follow,
                notified,
            } = self.backlog.remove(&thread).unwrap();
            match self
                .sender
                .try_send((thread.clone(), draft, anchor, follow))
            {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full((_, draft, anchor, follow))) => {
                    self.backlog.insert(
                        thread,
                        PendingSave {
                            draft,
                            anchor,
                            follow,
                            notified,
                        },
                    );
                    break;
                }
                Err(mpsc::error::TrySendError::Closed((_, draft, anchor, follow))) => {
                    self.backlog.insert(
                        thread.clone(),
                        PendingSave {
                            draft,
                            anchor,
                            follow,
                            notified: true,
                        },
                    );
                    self.closed = true;
                    return Some(thread);
                }
            }
        }
        None
    }
    // Pump persistence on every frontend turn; only its failure projection waits
    // for the main producer's reserved UI capacity.
    pub(super) fn pump_delivery(
        &mut self,
        failure: &mut Option<String>,
        updates: &BufferedUpdates,
    ) {
        if failure.is_none() {
            *failure = self.pump();
        }
        if updates.main_ready()
            && let Some(thread_id) = failure.take()
        {
            let rejected = updates.defer(UiUpdate::DraftSaveFailed {
                thread_id,
                message: "下書き保存workerが停止しました。本文を保持しています。終了時に保存失敗として報告します".into(),
            });
            assert!(
                rejected.is_none(),
                "reserved main capacity was consumed concurrently"
            );
        }
    }
    pub(super) fn threads(&self) -> impl Iterator<Item = &String> {
        self.backlog.keys()
    }
    pub(super) fn forget_threads(&mut self, deleted: &HashSet<String>) {
        self.backlog.retain(|thread, _| !deleted.contains(thread));
    }
    pub(super) async fn finish(self) -> tokio::task::JoinHandle<Result<(), ApiError>> {
        for (
            thread,
            PendingSave {
                draft,
                anchor,
                follow,
                ..
            },
        ) in self.backlog
        {
            let _ = self.sender.send((thread, draft, anchor, follow)).await;
        }
        drop(self.sender);
        self.worker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pumping_persists_backlog_while_ui_outbox_has_no_main_capacity() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let (results, _incoming) = mpsc::channel(1);
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let updates = BufferedUpdates::new(tx);
        // Receipt is deliberately critical; no raw UI or outbox capacity is freed.
        for revision in 0..512 {
            assert!(
                updates
                    .defer(UiUpdate::Receipt {
                        draft_id: "draft".into(),
                        revision,
                        thread_id: Some("thread".into()),
                        status: ReceiptStatus::Accepted,
                        message: None
                    })
                    .is_none()
            );
        }
        assert!(!updates.main_ready());
        let mut saver = DraftSaver::new(control.clone(), results);
        saver.enqueue(
            "thread".into(),
            DraftSnapshot {
                draft_id: "autosave".into(),
                text: "autosave despite stopped UI".into(),
                revision: 5,
                ..Default::default()
            },
            ViewAnchor::default(),
            true,
            false,
        );
        let mut failure = None;
        // This is the same unconditional helper called by the actual frontend
        // turn: gating pump on main_ready would leave this snapshot unsaved.
        saver.pump_delivery(&mut failure, &updates);
        assert!(failure.is_none());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let saved: bool = owner
                    .db
                    .call(|db| {
                        Ok(db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM draft WHERE id='autosave' AND revision=5)",
                            [],
                            |r| r.get(0),
                        )?)
                    })
                    .await
                    .unwrap();
                if saved {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!updates.main_ready());
        saver.finish().await.await.unwrap().unwrap();
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn closed_worker_projects_one_failure_across_ticks_and_keeps_newest_snapshot() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let (results, _incoming) = mpsc::channel(1);
        let mut saver = DraftSaver::new(control.clone(), results);
        saver.worker.abort();
        tokio::time::timeout(Duration::from_secs(2), saver.sender.closed())
            .await
            .unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.send(UiUpdate::RecoveryBell).unwrap();
        let updates = BufferedUpdates::new(tx);
        assert!(saver.enqueue(
            "thread".into(),
            DraftSnapshot {
                text: "first unsaved".into(),
                revision: 5,
                ..Default::default()
            },
            ViewAnchor::default(),
            true,
            false
        ));
        let mut failure = None;
        for _ in 0..600 {
            saver.pump_delivery(&mut failure, &updates);
        }
        assert!(saver.closed);
        assert_eq!(saver.backlog["thread"].draft.text, "first unsaved");
        assert!(saver.enqueue(
            "thread".into(),
            DraftSnapshot {
                text: "newest unsaved".into(),
                revision: 6,
                ..Default::default()
            },
            ViewAnchor::default(),
            true,
            false
        ));
        for _ in 0..600 {
            saver.pump_delivery(&mut failure, &updates);
        }
        assert_eq!(saver.backlog["thread"].draft.text, "newest unsaved");
        assert_eq!(saver.backlog["thread"].draft.revision, 6);
        assert!(matches!(rx.try_recv().unwrap(), UiUpdate::RecoveryBell));
        let mut notices = 0;
        for _ in 0..600 {
            updates.flush_pending();
            while let Ok(value) = rx.try_recv() {
                assert!(matches!(value, UiUpdate::DraftSaveFailed { .. }));
                notices += 1;
            }
        }
        assert_eq!(
            notices, 1,
            "closed worker must not fill the critical outbox on every tick"
        );
        assert!(failure.is_none());
        assert!(saver.enqueue(
            "later-thread".into(),
            DraftSnapshot {
                text: "another unsaved draft".into(),
                revision: 1,
                ..Default::default()
            },
            ViewAnchor::default(),
            true,
            false
        ));
        for _ in 0..600 {
            saver.pump_delivery(&mut failure, &updates);
        }
        updates.flush_pending();
        assert!(
            matches!(rx.try_recv().unwrap(), UiUpdate::DraftSaveFailed { thread_id, .. } if thread_id == "later-thread")
        );
        assert!(saver.enqueue(
            "later-thread".into(),
            DraftSnapshot {
                text: "latest other draft".into(),
                revision: 2,
                ..Default::default()
            },
            ViewAnchor::default(),
            true,
            false
        ));
        for _ in 0..600 {
            saver.pump_delivery(&mut failure, &updates);
            updates.flush_pending();
        }
        assert!(
            rx.try_recv().is_err(),
            "new revisions must not repeat the Thread warning"
        );
        assert_eq!(
            saver.backlog["later-thread"].draft.text,
            "latest other draft"
        );
        let joined = saver.finish().await.await.unwrap_err();
        assert!(joined.is_cancelled());
        let saved: i64 = owner
            .db
            .call(|db| Ok(db.query_row("SELECT count(*) FROM draft", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert_eq!(saved, 0);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn closed_worker_reports_its_unsaved_owner_and_cannot_return_success_on_shutdown() {
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let (results, _incoming) = mpsc::channel(1);
        let mut saver = DraftSaver::new(control.clone(), results);
        saver.worker.abort();
        tokio::time::timeout(Duration::from_secs(2), saver.sender.closed())
            .await
            .unwrap();
        assert!(saver.enqueue(
            "thread".into(),
            DraftSnapshot {
                text: "not saved".into(),
                ..Default::default()
            },
            ViewAnchor::default(),
            true,
            false
        ));
        assert_eq!(saver.pump(), Some("thread".into()));
        let failure = saver.finish().await.await.unwrap_err();
        assert!(failure.is_cancelled());
        let count: i64 = owner
            .db
            .call(|db| Ok(db.query_row("SELECT count(*) FROM draft", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(count, 0);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}
