//! Live process/driver registrations, cancellation, and handle liveness.
//! DB Run/Turn state remains authoritative; these maps contain owned handles.
#[cfg(test)]
use super::ForegroundRelease;
use super::{DriverSlot, ForegroundSlot, LiveShell};
use crate::control::driver::proactive::ThreadJob;
use crate::{protocol::ApiError, shell::ShellHandle};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, MutexGuard, Notify};
use tokio_util::sync::CancellationToken;

pub(super) struct RecoveryRewindOwnerGuard<'a> {
    _drivers: MutexGuard<'a, HashMap<String, DriverSlot>>,
    _shells: MutexGuard<'a, HashMap<String, LiveShell>>,
}

pub(super) enum StoppedRunBoundary {
    NoMatchingDriver,
    MatchingDriverActive,
    InactiveJobNeedsJoin,
    InactiveRunClear,
}

/// Only the caller whose `claim_join` set `Joining` returns the slot to `Idle`.
async fn return_joined_slot(slot: &DriverSlot) {
    *slot.proactive_job.lock().await = ThreadJob::Idle;
    slot.proactive_join_done.notify_waiters();
}

enum SlotTransition<'a> {
    Departed(&'a str),
    Completed(&'a Arc<Mutex<ThreadJob>>),
    Idle(&'a Arc<Mutex<ThreadJob>>),
}

#[derive(Default)]
pub(super) struct LiveRegistry {
    drivers: Mutex<HashMap<String, DriverSlot>>,
    shells: Mutex<HashMap<String, LiveShell>>,
    #[cfg(all(test, unix))]
    pub(super) test_registration: Mutex<()>,
}
impl LiveRegistry {
    /// Hold driver and shell publication closed across a recovery rewind commit.
    /// Any retained slot is refused; callers must not cancel or join here.
    pub(super) async fn recovery_rewind_owner_guard(
        &self,
        thread: &str,
        run: &str,
        thread_runs: &HashSet<String>,
    ) -> Option<RecoveryRewindOwnerGuard<'_>> {
        let drivers = self.drivers.lock().await;
        if drivers.contains_key(thread) {
            return None;
        }
        let shells = self.shells.lock().await;
        if shells.values().any(|shell| {
            shell
                .run_id
                .as_ref()
                .is_some_and(|id| thread_runs.contains(id) || id == run)
        }) {
            return None;
        }
        Some(RecoveryRewindOwnerGuard {
            _drivers: drivers,
            _shells: shells,
        })
    }
    /// The lookup, existing-slot notification and new-slot publication share one
    /// guard. Allocate driver resources only when no slot already owns Thread.
    #[cfg(test)]
    pub(super) async fn register_or_nudge_driver(
        &self,
        thread: &str,
        run: String,
        owner_stop: &CancellationToken,
    ) -> Option<DriverSlot> {
        let mut drivers = self.drivers.lock().await;
        self.register_or_nudge_driver_locked(&mut drivers, thread, run, owner_stop)
    }

    /// Select the live Run and publish its slot under one registry admission.
    /// A concurrent Stop either changes the DB state before this check or finds
    /// the published slot when it next acquires the registry.
    pub(super) async fn register_live_run_driver(
        &self,
        database: &crate::storage::DbWorker,
        thread: &str,
        owner_stop: &CancellationToken,
    ) -> Result<Option<DriverSlot>, crate::storage::StorageError> {
        if owner_stop.is_cancelled() {
            return Ok(None);
        }
        let mut drivers = self.drivers.lock().await;
        let target_thread = thread.to_owned();
        let run = database
            .call(move |db| {
                use rusqlite::OptionalExtension;
                Ok(db
                    .query_row(
                        "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
                        [target_thread],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?)
            })
            .await?;
        let Some(run) = run.filter(|_| !owner_stop.is_cancelled()) else {
            return Ok(None);
        };
        Ok(self.register_or_nudge_driver_locked(&mut drivers, thread, run, owner_stop))
    }

    fn register_or_nudge_driver_locked(
        &self,
        drivers: &mut HashMap<String, DriverSlot>,
        thread: &str,
        run: String,
        owner_stop: &CancellationToken,
    ) -> Option<DriverSlot> {
        if let Some(existing) = drivers.get(thread) {
            if existing.driver_active.load(Ordering::Acquire) {
                existing.nudge.notify_one();
                return None;
            }
            // A natural Run exit may leave one writer-owned proactive job for
            // this Thread. A later Run reuses that same slot/job reference.
            let slot = DriverSlot {
                id: uuid::Uuid::new_v4().to_string(),
                run_id: run,
                stop: owner_stop.child_token(),
                nudge: existing.nudge.clone(),
                foreground: Arc::new(ForegroundSlot::default()),
                turn: Arc::new(Mutex::new(None)),
                driver_active: Arc::new(AtomicBool::new(true)),
                proactive_job: existing.proactive_job.clone(),
                proactive_join_done: existing.proactive_join_done.clone(),
            };
            drivers.insert(thread.to_owned(), slot.clone());
            return Some(slot);
        }
        let slot = DriverSlot {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: run,
            stop: owner_stop.child_token(),
            nudge: Arc::new(Notify::new()),
            foreground: Arc::new(ForegroundSlot::default()),
            turn: Arc::new(Mutex::new(None)),
            driver_active: Arc::new(AtomicBool::new(true)),
            proactive_job: Arc::default(),
            proactive_join_done: Arc::new(Notify::new()),
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
        self.settle_slot(thread, SlotTransition::Departed(slot))
            .await
    }

    pub(super) async fn finish_proactive_join(&self, thread: &str, slot: &DriverSlot) {
        return_joined_slot(slot).await;
        self.prune_idle_background(thread, &slot.proactive_job)
            .await;
    }

    /// Cancel and join the Thread-owned job on Stop/error or owner shutdown.
    pub(super) async fn cancel_join_proactive_job(&self, thread: &str, slot: &DriverSlot) {
        let notified = slot.proactive_join_done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let mut jobs = slot.proactive_job.lock().await;
        let joining = matches!(*jobs, ThreadJob::Joining);
        let claimed = jobs.claim_join();
        drop(jobs);
        if joining {
            notified.await;
        } else if let Some(mut job) = claimed {
            job.cancel();
            let _ = job.join().await;
            self.finish_proactive_join(thread, slot).await;
        }
    }

    /// A stopped inactive Run can retain a Thread job that still needs the
    /// boundary. Claim its join while the exact current slot is fenced, then
    /// release registry/job locks before awaiting producer completion.
    pub(super) async fn cancel_join_inactive_proactive_job_for_run(
        &self,
        thread: &str,
        run: &str,
    ) -> bool {
        loop {
            let drivers = self.drivers.lock().await;
            let Some(slot) = drivers
                .get(thread)
                .filter(|slot| slot.run_id == run && !slot.driver_active.load(Ordering::Acquire))
            else {
                return false;
            };
            let slot = slot.clone();
            let notified = slot.proactive_join_done.clone().notified_owned();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut jobs = slot.proactive_job.lock().await;
            if matches!(*jobs, ThreadJob::Joining) {
                drop(jobs);
                drop(drivers);
                notified.await;
                continue;
            }
            let Some(mut job) = jobs.claim_join() else {
                return false;
            };
            job.cancel();
            drop(jobs);
            drop(drivers);
            let _ = job.join().await;
            self.finish_proactive_join(thread, &slot).await;
            return true;
        }
    }

    /// Revalidate no-driver Stop ownership at the boundary admission point.
    /// Callers drop the boundary before joining an inactive retained job.
    pub(super) async fn stopped_run_boundary_state(
        &self,
        thread: &str,
        run: &str,
    ) -> StoppedRunBoundary {
        let mut drivers = self.drivers.lock().await;
        let Some(slot) = drivers
            .get(thread)
            .filter(|slot| slot.run_id == run)
            .cloned()
        else {
            return StoppedRunBoundary::NoMatchingDriver;
        };
        if slot.driver_active.load(Ordering::Acquire) {
            return StoppedRunBoundary::MatchingDriverActive;
        }
        if !matches!(*slot.proactive_job.lock().await, ThreadJob::Idle) {
            return StoppedRunBoundary::InactiveJobNeedsJoin;
        }
        // A stopped inactive slot with no retained job cannot do more Run work.
        // Remove it at the same boundary cut so drain(None) cannot wait on its
        // otherwise-idle registry entry; a replacement Run is fenced by run ID.
        if drivers
            .get(thread)
            .is_some_and(|current| current.id == slot.id && current.run_id == run)
        {
            drivers.remove(thread);
        }
        StoppedRunBoundary::InactiveRunClear
    }

    /// Deletion is committed before this effect is applied; cancel only the deleted Thread's job.
    pub(super) async fn cancel_join_proactive_job_for_deleted_thread(&self, thread: &str) {
        let slot = self.drivers.lock().await.get(thread).cloned();
        if let Some(slot) = slot {
            self.cancel_join_proactive_job(thread, &slot).await;
        }
    }

    /// Drain jobs retained by naturally completed Runs before owner teardown.
    pub(super) async fn cancel_join_proactive_jobs(&self) {
        let slots = self
            .drivers
            .lock()
            .await
            .iter()
            .map(|(thread, slot)| (thread.clone(), slot.clone()))
            .collect::<Vec<_>>();
        for (thread, slot) in slots {
            self.cancel_join_proactive_job(&thread, &slot).await;
        }
    }

    pub(super) async fn complete_background(&self, thread: &str, job: &Arc<Mutex<ThreadJob>>) {
        self.settle_slot(thread, SlotTransition::Completed(job))
            .await;
    }

    pub(super) async fn prune_idle_background(&self, thread: &str, job: &Arc<Mutex<ThreadJob>>) {
        self.settle_slot(thread, SlotTransition::Idle(job)).await;
    }

    async fn settle_slot(&self, thread: &str, transition: SlotTransition<'_>) -> bool {
        let mut drivers = self.drivers.lock().await;
        let Some(slot) = drivers.get(thread) else {
            return false;
        };
        let mut job = slot.proactive_job.lock().await;
        match transition {
            SlotTransition::Departed(id) if slot.id == id => {
                slot.driver_active.store(false, Ordering::Release);
            }
            SlotTransition::Completed(exact) if Arc::ptr_eq(&slot.proactive_job, exact) => {
                job.complete();
            }
            SlotTransition::Idle(exact) if Arc::ptr_eq(&slot.proactive_job, exact) => {}
            _ => return false,
        }
        if slot.driver_active.load(Ordering::Acquire)
            || !matches!(*job, ThreadJob::Idle | ThreadJob::Completed(_))
        {
            return false;
        }
        drop(job);
        drivers.remove(thread);
        true
    }

    fn foreground_in(
        drivers: &HashMap<String, DriverSlot>,
        thread: &str,
        run: &str,
    ) -> Result<Option<String>, ()> {
        match drivers.get(thread) {
            Some(slot)
                if slot.run_id == run
                    && !slot.stop.is_cancelled()
                    && slot.driver_active.load(Ordering::Acquire) =>
            {
                Ok(slot.foreground.waiting_execution())
            }
            Some(_) => Err(()),
            None => Ok(None),
        }
    }

    /// Called on the DB worker. Never wait for a driver lock there: some
    /// lifecycle transitions hold it across their database commit.
    pub(super) fn foreground_observation(
        &self,
        thread: &str,
        run: &str,
    ) -> Result<Option<String>, ()> {
        let drivers = self.drivers.try_lock().map_err(|_| ())?;
        Self::foreground_in(&drivers, thread, run)
    }

    /// Project only returned rows, after the saved read releases its transaction.
    pub(super) async fn project_shell_foreground<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a mut serde_json::Value>,
    ) {
        let drivers = self.drivers.lock().await;
        let mut by_run = HashMap::new();
        for row in rows {
            if row["state"] != "running" {
                continue;
            }
            let foreground = match (
                row["thread_id"].as_str(),
                row["run_id"].as_str(),
                row["turn_id"].as_str(),
                row["execution_id"].as_str(),
            ) {
                (Some(thread), Some(run), Some(_), Some(execution)) => {
                    by_run
                        .entry((thread.to_owned(), run.to_owned()))
                        .or_insert_with(|| {
                            Self::foreground_in(&drivers, thread, run).ok().flatten()
                        })
                        .as_deref()
                        == Some(execution)
                }
                _ => false,
            };
            row["foreground"] = foreground.into();
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

    pub(super) async fn background(
        &self,
        thread: &str,
        target: Option<(&str, &str)>,
    ) -> Result<String, ApiError> {
        let drivers = self.drivers.lock().await;
        drivers
            .get(thread)
            .filter(|slot| {
                !slot.stop.is_cancelled() && target.is_none_or(|(run, _)| slot.run_id == run)
            })
            .and_then(|slot| {
                slot.foreground
                    .release_matching(target.map(|(_, execution)| execution))
            })
            .ok_or_else(|| super::background_refusal("not_foreground"))
    }

    pub(super) async fn register_shell(&self, execution: String, shell: LiveShell) {
        #[cfg(all(test, unix))]
        let _registration = self.test_registration.lock().await;
        self.shells.lock().await.insert(execution, shell);
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
            .filter(|shell| shell.run_id.as_deref() == Some(run))
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
            for shell in shells
                .values()
                .filter(|shell| shell.run_id.as_ref().is_some_and(|id| runs.contains(id)))
            {
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
            .any(|shell| shell.run_id.as_ref().is_some_and(|id| runs.contains(id)));
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_kicks_publish_one_slot_and_old_completion_cannot_remove_a_replacement() {
        let registry = Arc::new(LiveRegistry::default());
        let stop = CancellationToken::new();
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let spawn_kick = || {
            let registry = registry.clone();
            let stop = stop.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                registry
                    .register_or_nudge_driver("thread", "run".into(), &stop)
                    .await
            })
        };
        let first = spawn_kick();
        let second = spawn_kick();
        barrier.wait().await;
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert_eq!(
            usize::from(first.is_some()) + usize::from(second.is_some()),
            1
        );
        let first = first.or(second).unwrap();
        assert!(registry.remove_driver_if_current("thread", &first.id).await);
        let replacement = registry
            .register_or_nudge_driver("thread", "replacement".into(), &stop)
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
    async fn natural_run_exit_reattaches_the_same_thread_owned_proactive_job() {
        let registry = Arc::new(LiveRegistry::default());
        let owner_stop = CancellationToken::new();
        let first = registry
            .register_or_nudge_driver("thread", "run-a".into(), &owner_stop)
            .await
            .unwrap();
        let old_run_stop = first.stop.clone();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let registry_for_job = registry.clone();
        let job_for_job = first.proactive_job.clone();
        let task = tokio::spawn(async move {
            let _ = release_rx.await;
            registry_for_job
                .prune_idle_background("thread", &job_for_job)
                .await;
            let _ = finished_tx.send(());
            Ok(
                crate::control::driver::proactive::ProactiveOutcome::Finished {
                    path_changed: false,
                },
            )
        });
        *first.proactive_job.lock().await = ThreadJob::Retained(
            crate::control::driver::proactive::BackgroundJob::test_started(
                first.stop.clone(),
                task,
            ),
        );
        assert!(
            !registry.remove_driver_if_current("thread", &first.id).await,
            "natural retirement preserves the Thread slot while its job is unjoined"
        );
        release_tx.send(()).unwrap();
        finished_rx.await.unwrap();
        assert!(
            registry.test_drivers().lock().await.contains_key("thread"),
            "completion alone cannot prune a result that Run B must join"
        );
        crate::test_watchdog::watchdog("proactive job finishes before Run B reattaches", async {
            loop {
                let finished = first
                    .proactive_job
                    .lock()
                    .await
                    .retained()
                    .is_some_and(crate::control::driver::proactive::BackgroundJob::is_finished);
                if finished {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let second = registry
            .register_or_nudge_driver("thread", "run-b".into(), &owner_stop)
            .await
            .expect("a natural Run exit permits a new driver on the same Thread");
        assert!(Arc::ptr_eq(&first.proactive_job, &second.proactive_job));
        assert!(second.proactive_job.lock().await.retained().is_some());
        assert!(
            !old_run_stop.is_cancelled(),
            "natural retirement is not Stop"
        );
        assert!(!second.stop.is_cancelled());
        assert!(
            second
                .proactive_job
                .lock()
                .await
                .retained()
                .unwrap()
                .is_finished(),
            "Run B reattaches the completed-but-unjoined Run A job"
        );
        let mut job = second.proactive_job.lock().await.take_finished().unwrap();
        assert!(matches!(
            job.join().await.unwrap(),
            Ok(
                crate::control::driver::proactive::ProactiveOutcome::Finished {
                    path_changed: false
                }
            )
        ));
        registry
            .prune_idle_background("thread", &second.proactive_job)
            .await;
        assert!(
            registry
                .foreground_for_run("thread", "run-b")
                .await
                .is_some()
        );
        assert!(
            !registry.remove_driver_if_current("thread", &first.id).await,
            "late Run-A cleanup cannot remove Run-B's Thread slot"
        );
        assert!(
            registry
                .remove_driver_if_current("thread", &second.id)
                .await,
            "normal retirement removes the slot after the background job is joined"
        );
        assert!(registry.is_empty().await);
    }

    #[tokio::test]
    async fn stop_cancels_and_joins_the_job_in_the_shared_thread_slot() {
        let registry = LiveRegistry::default();
        let owner_stop = CancellationToken::new();
        let slot = registry
            .register_or_nudge_driver("thread", "run".into(), &owner_stop)
            .await
            .unwrap();
        let job_cancel = CancellationToken::new();
        let observed_join = Arc::new(AtomicBool::new(false));
        let task_cancel = job_cancel.clone();
        let joined = observed_join.clone();
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            joined.store(true, Ordering::Release);
            Ok(crate::control::driver::proactive::ProactiveOutcome::NotStarted)
        });
        *slot.proactive_job.lock().await = ThreadJob::Retained(
            crate::control::driver::proactive::BackgroundJob::test_started(job_cancel, task),
        );

        assert!(registry.cancel_run("thread", "run").await);
        assert!(slot.stop.is_cancelled());
        registry.cancel_join_proactive_job("thread", &slot).await;
        assert!(observed_join.load(Ordering::Acquire));
        assert!(matches!(*slot.proactive_job.lock().await, ThreadJob::Idle));
        assert!(matches!(*slot.proactive_job.lock().await, ThreadJob::Idle));
        assert!(registry.remove_driver_if_current("thread", &slot.id).await);
        assert!(registry.is_empty().await);
    }

    #[tokio::test]
    async fn owner_shutdown_joins_a_job_retained_after_natural_run_exit() {
        let registry = LiveRegistry::default();
        let owner_stop = CancellationToken::new();
        let slot = registry
            .register_or_nudge_driver("thread", "run-a".into(), &owner_stop)
            .await
            .unwrap();
        let job_cancel = CancellationToken::new();
        let joined = Arc::new(AtomicBool::new(false));
        let task_cancel = job_cancel.clone();
        let observed = joined.clone();
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            observed.store(true, Ordering::Release);
            Ok(crate::control::driver::proactive::ProactiveOutcome::NotStarted)
        });
        *slot.proactive_job.lock().await = ThreadJob::Retained(
            crate::control::driver::proactive::BackgroundJob::test_started(job_cancel, task),
        );
        assert!(!registry.remove_driver_if_current("thread", &slot.id).await);

        registry.cancel_join_proactive_jobs().await;

        assert!(joined.load(Ordering::Acquire));
        assert!(matches!(*slot.proactive_job.lock().await, ThreadJob::Idle));
        assert!(matches!(*slot.proactive_job.lock().await, ThreadJob::Idle));
        assert!(registry.is_empty().await);
    }

    #[tokio::test]
    async fn a_joining_job_keeps_its_slot_owned_until_the_joiner_returns_it() {
        crate::test_watchdog::watchdog("joining state hands off to every reader", async {
            let registry = Arc::new(LiveRegistry::default());
            let owner_stop = CancellationToken::new();
            let slot = registry
                .register_or_nudge_driver("thread", "run".into(), &owner_stop)
                .await
                .unwrap();
            let job_cancel = CancellationToken::new();
            let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            let task_cancel = job_cancel.clone();
            let task = tokio::spawn(async move {
                task_cancel.cancelled().await;
                let _ = cancelled_tx.send(());
                let _ = release_rx.await;
                Ok(crate::control::driver::proactive::ProactiveOutcome::NotStarted)
            });
            *slot.proactive_job.lock().await = ThreadJob::Retained(
                crate::control::driver::proactive::BackgroundJob::test_started(job_cancel, task),
            );
            assert!(
                !registry.remove_driver_if_current("thread", &slot.id).await,
                "natural Run exit retains the unjoined job and its slot"
            );
            let stop = tokio::spawn({
                let registry = registry.clone();
                async move {
                    registry
                        .cancel_join_inactive_proactive_job_for_run("thread", "run")
                        .await
                }
            });
            cancelled_rx.await.unwrap();
            assert!(matches!(
                *slot.proactive_job.lock().await,
                ThreadJob::Joining
            ));
            assert!(matches!(
                registry.stopped_run_boundary_state("thread", "run").await,
                StoppedRunBoundary::InactiveJobNeedsJoin
            ));
            registry
                .prune_idle_background("thread", &slot.proactive_job)
                .await;
            assert!(!registry.remove_driver_if_current("thread", &slot.id).await);
            let shutdown = tokio::spawn({
                let registry = registry.clone();
                async move { registry.cancel_join_proactive_jobs().await }
            });
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert!(
                !shutdown.is_finished(),
                "owner shutdown waits for the Stop joiner instead of skipping the job"
            );
            assert!(registry.test_drivers().lock().await.contains_key("thread"));
            release_tx.send(()).unwrap();
            assert!(stop.await.unwrap(), "Stop joined the originating job");
            shutdown.await.unwrap();
            assert!(matches!(*slot.proactive_job.lock().await, ThreadJob::Idle));
            assert!(registry.is_empty().await);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn ordinary_kick_preserves_foreground_until_explicit_background() {
        let registry = LiveRegistry::default();
        let stop = CancellationToken::new();
        assert_eq!(
            registry.background("missing", None).await.unwrap_err().code,
            crate::protocol::ErrorCode::InvalidState
        );
        let slot = registry
            .register_or_nudge_driver("thread", "run".into(), &stop)
            .await
            .unwrap();
        assert_eq!(
            registry.background("thread", None).await.unwrap_err().code,
            crate::protocol::ErrorCode::InvalidState
        );
        let mut call = slot.foreground.register("execution");
        assert!(
            registry
                .register_or_nudge_driver("thread", "ignored-run".into(), &stop)
                .await
                .is_none()
        );
        crate::test_watchdog::watchdog("normal driver nudge", slot.nudge.notified())
            .await
            .unwrap();
        assert!(
            slot.foreground.waiting_execution().as_deref() == Some("execution"),
            "quiet kick nudges without releasing foreground"
        );
        registry.background("thread", None).await.unwrap();
        assert_eq!(
            crate::test_watchdog::watchdog("explicit Background release", call.recv())
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
