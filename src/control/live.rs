//! Live process/driver registrations, cancellation, and handle liveness.
//! DB Run/Turn state remains authoritative; these maps contain owned handles.
use super::{DriverSlot, ForegroundRelease, ForegroundSlot, LiveShell};
use crate::{
    protocol::{ApiError, ErrorCode},
    shell::ShellHandle,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct LiveRegistry {
    drivers: Mutex<HashMap<String, DriverSlot>>,
    shells: Mutex<HashMap<String, LiveShell>>,
}
impl LiveRegistry {
    /// The lookup, existing-slot notification and new-slot publication share one
    /// guard. Allocate driver resources only when no slot already owns Thread.
    pub(super) async fn register_or_nudge_driver(
        &self,
        thread: &str,
        run: String,
        owner_stop: &CancellationToken,
        interrupt: bool,
    ) -> Option<DriverSlot> {
        let mut drivers = self.drivers.lock().await;
        if let Some(slot) = drivers.get(thread) {
            slot.nudge.notify_one();
            if interrupt {
                slot.foreground.release(ForegroundRelease::AdditionalInput);
            }
            return None;
        }
        let slot = DriverSlot {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: run,
            stop: owner_stop.child_token(),
            nudge: Arc::new(Notify::new()),
            foreground: Arc::new(ForegroundSlot::default()),
            turn: Arc::new(Mutex::new(None)),
        };
        drivers.insert(thread.to_owned(), slot.clone());
        Some(slot)
    }

    /// Keep absent/replacement driver publication fenced through the short DB commit.
    pub(super) async fn admit_turn_interrupt(
        &self,
        control: &super::Control,
        thread: &str,
        turn: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let drivers = self.drivers.lock().await;
        super::turn_interrupt::admit_locked(control, thread, turn, drivers.get(thread)).await
    }

    pub(super) async fn remove_driver_if_current(&self, thread: &str, slot: &str) -> bool {
        let mut drivers = self.drivers.lock().await;
        if drivers.get(thread).is_some_and(|entry| entry.id == slot) {
            drivers.remove(thread);
            true
        } else {
            false
        }
    }

    pub(super) async fn foreground_for_run(
        &self,
        thread: &str,
        run: &str,
    ) -> Option<Arc<ForegroundSlot>> {
        self.drivers
            .lock()
            .await
            .get(thread)
            .filter(|slot| slot.run_id == run)
            .map(|slot| slot.foreground.clone())
    }

    pub(super) async fn background(&self, thread: &str) -> Result<(), ApiError> {
        let drivers = self.drivers.lock().await;
        let slot = drivers.get(thread).ok_or_else(|| {
            ApiError::new(ErrorCode::InvalidState, "No foreground work is running")
        })?;
        if !slot.foreground.release(ForegroundRelease::Background) {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "No foreground shell is being awaited",
            ));
        }
        Ok(())
    }

    pub(super) async fn register_shell(&self, execution: String, shell: LiveShell) {
        self.shells.lock().await.insert(execution, shell);
    }

    pub(super) async fn record_output_commit(&self, execution: &str, at: std::time::Instant) {
        if let Some(shell) = self.shells.lock().await.get_mut(execution) {
            shell.last_committed_output = Some(at);
        }
    }
    pub(super) async fn output_markers(
        &self,
        executions: &[String],
    ) -> Vec<(String, std::time::Instant)> {
        if executions.is_empty() {
            return Vec::new();
        }
        let shells = self.shells.lock().await;
        executions
            .iter()
            .filter_map(|id| {
                shells
                    .get(id)
                    .and_then(|shell| shell.last_committed_output)
                    .map(|at| (id.clone(), at))
            })
            .collect()
    }

    pub(super) async fn remove_shell(&self, execution: &str) {
        self.shells.lock().await.remove(execution);
    }

    pub(super) async fn clone_shell_handle(&self, execution: &str) -> Option<ShellHandle> {
        self.shells
            .lock()
            .await
            .get(execution)
            .map(|shell| shell.handle.clone())
    }

    /// Stop while holding the shell guard, so a caller retaining the runtime
    /// intent guard preserves the existing intent -> registration lock order.
    pub(super) async fn stop_shell(&self, execution: &str) {
        if let Some(shell) = self.shells.lock().await.get(execution) {
            shell.handle.stop();
        }
    }

    #[cfg(test)]
    pub(super) fn test_drivers(&self) -> &Mutex<HashMap<String, DriverSlot>> {
        &self.drivers
    }

    #[cfg(all(test, unix))]
    pub(super) fn test_shells(&self) -> &Mutex<HashMap<String, LiveShell>> {
        &self.shells
    }
    pub(super) async fn cancel_run(&self, thread: &str, run: &str) -> bool {
        let active = {
            let drivers = self.drivers.lock().await;
            if let Some(slot) = drivers.get(thread).filter(|slot| slot.run_id == run) {
                slot.stop.cancel();
                true
            } else {
                false
            }
        };
        for shell in self
            .shells
            .lock()
            .await
            .values()
            .filter(|shell| shell.run_id == run)
        {
            shell.handle.stop();
        }
        active
    }
    pub(super) async fn cancel_owned(&self, runs: &HashSet<String>, current_thread: Option<&str>) {
        {
            let drivers = self.drivers.lock().await;
            for (thread, slot) in drivers.iter() {
                if runs.contains(&slot.run_id) && current_thread != Some(thread.as_str()) {
                    slot.stop.cancel();
                }
            }
        }
        {
            let shells = self.shells.lock().await;
            for shell in shells.values().filter(|shell| runs.contains(&shell.run_id)) {
                shell.handle.stop();
            }
        }
    }
    pub(super) async fn has_owned(
        &self,
        runs: &HashSet<String>,
        current_thread: Option<&str>,
    ) -> bool {
        let drivers_alive = self.drivers.lock().await.iter().any(|(thread, slot)| {
            runs.contains(&slot.run_id) && current_thread != Some(thread.as_str())
        });
        let shells_alive = self
            .shells
            .lock()
            .await
            .values()
            .any(|shell| runs.contains(&shell.run_id));
        drivers_alive || shells_alive
    }
    pub(super) async fn is_empty(&self) -> bool {
        // Do not retain either map guard while acquiring the other.
        let drivers_empty = self.drivers.lock().await.is_empty();
        let shells_empty = self.shells.lock().await.is_empty();
        drivers_empty && shells_empty
    }
    pub(super) async fn stop_shells(&self) {
        for shell in self.shells.lock().await.values() {
            shell.handle.stop();
        }
    }
    pub(super) fn try_stop_shells(&self) {
        if let Ok(shells) = self.shells.try_lock() {
            for shell in shells.values() {
                shell.handle.stop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_kicks_publish_one_slot_and_old_completion_cannot_remove_a_replacement() {
        let registry = Arc::new(LiveRegistry::default());
        let stop = CancellationToken::new();
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let spawn_kick = |interrupt| {
            let registry = registry.clone();
            let stop = stop.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                registry
                    .register_or_nudge_driver("thread", "run".into(), &stop, interrupt)
                    .await
            })
        };
        let first = spawn_kick(false);
        let second = spawn_kick(true);
        barrier.wait().await;
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert_eq!(
            usize::from(first.is_some()) + usize::from(second.is_some()),
            1
        );
        let first = first.or(second).unwrap();
        assert!(registry.remove_driver_if_current("thread", &first.id).await);
        let replacement = registry
            .register_or_nudge_driver("thread", "replacement".into(), &stop, false)
            .await
            .unwrap();
        assert!(!registry.remove_driver_if_current("thread", &first.id).await);
        assert!(
            !registry.cancel_run("thread", "run").await,
            "stale Run cancellation must not cancel the replacement driver"
        );
        assert!(!replacement.stop.is_cancelled());
        let runs = HashSet::from(["replacement".to_owned()]);
        registry.cancel_owned(&runs, Some("thread")).await;
        assert!(
            !replacement.stop.is_cancelled(),
            "owned drain excludes the current driver"
        );
        assert!(!registry.has_owned(&runs, Some("thread")).await);
        registry.cancel_owned(&runs, None).await;
        assert!(replacement.stop.is_cancelled());
        assert!(
            registry
                .remove_driver_if_current("thread", &replacement.id)
                .await
        );
        assert!(registry.is_empty().await);
    }

    #[tokio::test]
    async fn quiet_and_interrupt_kicks_preserve_the_slot_and_background_targets_its_live_call() {
        let registry = LiveRegistry::default();
        let stop = CancellationToken::new();
        assert_eq!(
            registry.background("missing").await.unwrap_err().code,
            ErrorCode::InvalidState
        );
        let slot = registry
            .register_or_nudge_driver("thread", "run".into(), &stop, false)
            .await
            .unwrap();
        assert_eq!(
            registry.background("thread").await.unwrap_err().code,
            ErrorCode::InvalidState
        );
        let mut call = slot.foreground.register("execution");
        assert!(
            registry
                .register_or_nudge_driver("thread", "ignored-run".into(), &stop, false)
                .await
                .is_none()
        );
        tokio::time::timeout(Duration::from_secs(1), slot.nudge.notified())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), call.recv())
                .await
                .is_err(),
            "quiet kick nudges without releasing foreground"
        );
        assert!(
            registry
                .register_or_nudge_driver("thread", "ignored-run".into(), &stop, true)
                .await
                .is_none()
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), call.recv())
                .await
                .unwrap(),
            Some(ForegroundRelease::AdditionalInput)
        );
        registry.background("thread").await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), call.recv())
                .await
                .unwrap(),
            Some(ForegroundRelease::Background)
        );
        assert!(
            registry.remove_driver_if_current("thread", &slot.id).await,
            "nudge never replaces a registered slot"
        );
    }
}
