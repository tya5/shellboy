//! The adapter owns shutdown; Core only admits its saved request and signals it.
use super::*;

impl Control {
    pub(crate) fn owner_shutdown_signal(&self) -> CancellationToken {
        self.owner_shutdown
            .get_or_init(CancellationToken::new)
            .clone()
    }
    pub(crate) fn freeze_owner_admission(&self) {
        self.shell.close_start_admission();
        self.admission.close();
        self.changed.notify_waiters();
    }
    pub(crate) async fn stopping(&self) {
        self.stop.cancelled().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DataOwner;
    use tokio::sync::{Semaphore, mpsc, oneshot};

    async fn fixture(root: &std::path::Path) -> (DataOwner, Arc<Control>) {
        let config = root.join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = DataOwner::start(Some(config), Some(root.join("data")), root.into())
            .await
            .unwrap();
        let control = Control::new(&owner, root.into());
        (owner, control)
    }
    fn request(control: &Control, instance: String) -> Request {
        Request::new(
            format!(
                "{}:{}",
                control.store.operation_epoch.current(),
                uuid::Uuid::new_v4()
            ),
            Command::OwnerShutdown(OwnerShutdownArgs {
                instance_id: instance,
            }),
        )
    }
    async fn receipts(control: &Control) -> i64 {
        control
            .store
            .database
            .call(|db| {
                db.query_row("SELECT COUNT(*) FROM operation_receipt", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn shutdown_rejects_tui_and_stale_instance_before_receipt_and_replays_without_effect() {
        crate::test_watchdog::watchdog("shutdown scope and saved acceptance", async {
            let root = tempfile::tempdir().unwrap();
            let (owner, control) = fixture(root.path()).await;
            let correct = request(&control, owner.instance_id.clone());
            let rejected = control.request(correct.clone()).await;
            assert_eq!(rejected.error.unwrap().code, ErrorCode::InvalidState);
            assert_eq!(receipts(&control).await, 0);
            let stop = control.owner_shutdown_signal();
            let stale = control.request(request(&control, "old-owner".into())).await;
            assert_eq!(stale.error.unwrap().code, ErrorCode::ProtocolMismatch);
            assert_eq!(receipts(&control).await, 0);
            let first = control.store.execute(correct.clone(), None).await.unwrap();
            assert!(first.owner_shutdown);
            let replay = control.store.execute(correct, None).await.unwrap();
            assert!(!replay.owner_shutdown);
            assert_eq!(first.result, replay.result);
            assert_eq!(receipts(&control).await, 1);
            assert!(!stop.is_cancelled());
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn shutdown_handoff_releases_both_admissions_before_session_signal() {
        crate::test_watchdog::watchdog("shutdown receipt/reply/session boundary", async {
            let root = tempfile::tempdir().unwrap();
            let (owner, control) = fixture(root.path()).await;
            let stop = control.owner_shutdown_signal();
            let (send, receive) = mpsc::channel(1);
            let dispatch = tokio::spawn(control.clone().serve(receive));
            let slots = Arc::new(Semaphore::new(1));
            let (reply, observation) = oneshot::channel();
            send.send(Invocation {
                request: request(&control, owner.instance_id.clone()),
                reply,
                disconnected: CancellationToken::new(),
                admission: slots.clone().acquire_owned().await.unwrap(),
            })
            .await
            .unwrap();
            let response = observation.await.unwrap();
            assert!(response.ok);
            assert_eq!(response.result.unwrap()["shutdown_complete"], false);
            assert_eq!(receipts(&control).await, 1);
            stop.cancelled().await;
            assert!(control.admission.is_closed());
            assert!(control.admission.is_idle());
            assert_eq!(slots.available_permits(), 1);
            control.shutdown().await.unwrap();
            drop(send);
            dispatch.await.unwrap();
            owner.shutdown().await.unwrap();
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn accepted_shutdown_receipt_is_not_replayed_as_restart_intent() {
        crate::test_watchdog::watchdog("saved shutdown acceptance after reopen", async {
            let root = tempfile::tempdir().unwrap();
            let (owner, control) = fixture(root.path()).await;
            let _stop = control.owner_shutdown_signal();
            let old = request(&control, owner.instance_id.clone());
            let accepted = control.store.execute(old.clone(), None).await.unwrap();
            assert!(accepted.owner_shutdown);
            assert_eq!(receipts(&control).await, 1);
            // Simulate death after acceptance and before effect adoption.
            control.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
            drop(control);
            let (owner, fresh) = fixture(root.path()).await;
            let stop = fresh.owner_shutdown_signal();
            fresh.start().await.unwrap();
            assert_eq!(receipts(&fresh).await, 1);
            let rejected = fresh.request(old).await;
            assert_eq!(rejected.error.unwrap().code, ErrorCode::ProtocolMismatch);
            assert!(!stop.is_cancelled());
            assert!(!fresh.admission.is_closed());
            assert_eq!(receipts(&fresh).await, 1);
            fresh.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        })
        .await
        .unwrap();
    }
}
