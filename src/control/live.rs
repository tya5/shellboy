//! Live process/driver registrations, cancellation, and handle liveness.
//! DB Run/Turn state remains authoritative; these maps contain owned handles.
use super::{DriverSlot, LiveShell};
use std::collections::{HashMap, HashSet};
use tokio::sync::Mutex;

#[derive(Default)]
pub(super) struct LiveRegistry {
    // Control keeps its atomic registration/replacement guards at the spawn
    // boundary. Cancellation and combined liveness stay together here.
    pub(super) drivers: Mutex<HashMap<String, DriverSlot>>,
    pub(super) shells: Mutex<HashMap<String, LiveShell>>,
}
impl LiveRegistry {
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
