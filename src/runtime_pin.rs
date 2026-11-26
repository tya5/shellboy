//! Runtime pins: `runtime_pin` rows that keep a resource out of retention
//! while this owner instance reads or sends it.
//!
//! One [`PinLease`] owns every row whose id starts with its prefix. A lease is
//! admitted only with one of [`LEASE_SLOTS`] finalization slots, so its
//! release always has capacity; it is created before the job that inserts its
//! rows, and that job holds the lease's insert token until it has run (or is
//! dropped unrun). Dropping the lease sends its release, with the slot, to the
//! DB worker, which wakes for it like for a job: nothing is spawned and `Drop`
//! never retries. The worker releases a lease once its insert job is done, in
//! its own transaction; the slot is freed when that commits. A failed release
//! is recorded as a database failure in diagnostics and retried by the worker
//! at its next idle maintenance point, not at every job. An owner that settles
//! its own rows in one transaction (a failed clipboard save) closes the lease
//! with [`PinLease::settled`] instead. Rows of an earlier instance are removed
//! at startup.
use crate::maintenance::{Signals, Work};
use crate::storage::{DbWorker, StorageError};
use rusqlite::{Connection, params};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

/// Outstanding leases, each holding one finalization slot until its release
/// commits. Acquiring a lease waits for a slot.
pub(crate) const LEASE_SLOTS: usize = 1024;
/// Releases committed per transaction.
const RELEASE_BATCH: usize = 128;

pub(crate) struct PinLease {
    sender: mpsc::Sender<Release>,
    prefix: String,
    /// Shared with the inserting job while it can still run.
    insert: Arc<()>,
    slot: Option<OwnedSemaphorePermit>,
}

impl PinLease {
    /// Admit a lease with its finalization slot.
    pub(crate) async fn acquire(database: &DbWorker) -> Result<Self, StorageError> {
        let releases = database.pin_releases();
        let slot = releases
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StorageError::Closed)?;
        Ok(Self {
            sender: releases.sender.clone(),
            prefix: uuid::Uuid::new_v4().to_string(),
            insert: Arc::new(()),
            slot: Some(slot),
        })
    }
    /// Id prefix for the rows this lease owns; [`pin_id`] makes each id.
    pub(crate) fn prefix(&self) -> String {
        self.prefix.clone()
    }
    /// Move this into the inserting job's closure; the release waits for it.
    pub(crate) fn insert_token(&self) -> Arc<()> {
        self.insert.clone()
    }
    /// No row was inserted, or the owner deleted them in its own transaction:
    /// close the lease without a release.
    pub(crate) fn settled(mut self) {
        self.slot = None;
    }
}

impl Drop for PinLease {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        // The channel holds `LEASE_SLOTS` and every queued release holds a
        // slot, so it is never full. A closed worker means the owner is
        // stopping; startup removes rows of this instance then.
        let _ = self.sender.try_send(Release {
            prefix: std::mem::take(&mut self.prefix),
            insert: std::mem::replace(&mut self.insert, Arc::new(())),
            _slot: slot,
        });
    }
}

/// The `n`th row id under a lease prefix.
pub(crate) fn pin_id(prefix: &str, n: usize) -> String {
    format!("{prefix}.{n}")
}

/// Owner-side delete of a lease's rows, for an owner that settles them itself.
pub(crate) fn delete_pins_tx(tx: &Connection, prefix: &str) -> rusqlite::Result<usize> {
    tx.execute(
        "DELETE FROM runtime_pin WHERE id>?1 AND id<?2",
        params![format!("{prefix}."), format!("{prefix}/")],
    )
}

struct Release {
    prefix: String,
    insert: Arc<()>,
    _slot: OwnedSemaphorePermit,
}
impl Release {
    /// Its inserting job has run or will never run.
    fn ready(&self) -> bool {
        Arc::strong_count(&self.insert) == 1
    }
}

/// The sending side, shared by every lease of one DB worker.
pub(crate) struct Releases {
    slots: Arc<Semaphore>,
    sender: mpsc::Sender<Release>,
}

/// The DB worker's side: received releases waiting for their insert job, and
/// failed ones waiting for the next maintenance point. Both are bounded by
/// [`LEASE_SLOTS`], since every entry still holds its slot.
pub(crate) struct ReleaseWork {
    receiver: mpsc::Receiver<Release>,
    waiting: Vec<Release>,
    failed: Vec<Release>,
    last_retry: Option<Instant>,
}

pub(crate) fn channel() -> (Releases, ReleaseWork) {
    let (sender, receiver) = mpsc::channel(LEASE_SLOTS);
    (
        Releases {
            slots: Arc::new(Semaphore::new(LEASE_SLOTS)),
            sender,
        },
        ReleaseWork {
            receiver,
            waiting: Vec::new(),
            failed: Vec::new(),
            last_retry: None,
        },
    )
}

impl ReleaseWork {
    /// The worker's wake: the next release sent by a dropped lease.
    pub(crate) async fn recv(&mut self) -> bool {
        match self.receiver.recv().await {
            Some(release) => {
                self.waiting.push(release);
                true
            }
            None => false,
        }
    }
    /// DB worker thread: release every received lease whose insert job is
    /// done, in transactions of at most [`RELEASE_BATCH`].
    pub(crate) fn service(&mut self, db: &mut Connection, signals: &Signals, failed: impl Fn()) {
        while let Ok(release) = self.receiver.try_recv() {
            self.waiting.push(release);
        }
        loop {
            let mut batch = Vec::new();
            let mut index = 0;
            while index < self.waiting.len() && batch.len() < RELEASE_BATCH {
                if self.waiting[index].ready() {
                    batch.push(self.waiting.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            if batch.is_empty() {
                return;
            }
            if !self.commit(db, signals, batch, &failed) {
                return;
            }
        }
    }
    /// DB worker thread, at its idle maintenance point: retry failed releases
    /// at most once per `interval`.
    pub(crate) fn retry_failed(
        &mut self,
        db: &mut Connection,
        signals: &Signals,
        now: Instant,
        interval: std::time::Duration,
        failed: impl Fn(),
    ) {
        if self.failed.is_empty()
            || self
                .last_retry
                .is_some_and(|last| now.duration_since(last) < interval)
        {
            return;
        }
        self.last_retry = Some(now);
        let count = self.failed.len().min(RELEASE_BATCH);
        let batch: Vec<Release> = self.failed.drain(..count).collect();
        self.commit(db, signals, batch, &failed);
    }
    fn commit(
        &mut self,
        db: &mut Connection,
        signals: &Signals,
        batch: Vec<Release>,
        failed: &impl Fn(),
    ) -> bool {
        match release_tx(db, &batch) {
            Ok(managed) => {
                if managed {
                    // Released managed pins can make images and raw collectable.
                    signals.mark_all(&[Work::Managed, Work::Raw]);
                }
                true
            }
            Err(_) => {
                failed();
                self.failed.extend(batch);
                false
            }
        }
    }
}

/// Returns whether any managed-file pin was released.
fn release_tx(db: &mut Connection, batch: &[Release]) -> rusqlite::Result<bool> {
    let tx = db.transaction()?;
    let mut managed = false;
    for release in batch {
        let kinds = tx
            .prepare_cached(
                "DELETE FROM runtime_pin WHERE id>?1 AND id<?2 RETURNING resource_kind",
            )?
            .query_map(
                params![
                    format!("{}.", release.prefix),
                    format!("{}/", release.prefix)
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        managed |= kinds.iter().any(|kind| kind == "managed_file");
    }
    tx.commit()?;
    Ok(managed)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pinned_lease(db: &DbWorker) -> PinLease {
        let lease = PinLease::acquire(db).await.unwrap();
        let (id, insert) = (pin_id(&lease.prefix(), 0), lease.insert_token());
        db.call(move |db| {
            let _insert = insert;
            db.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES(?1,'fixture','shell_output','e',0,'now')", [id])?;
            Ok(())
        })
        .await
        .unwrap();
        lease
    }

    fn pins(path: &std::path::Path) -> i64 {
        // A separate connection: observing must not be a job on the worker.
        Connection::open(path)
            .unwrap()
            .query_row("SELECT count(*) FROM runtime_pin", [], |r| r.get(0))
            .unwrap()
    }

    /// A lease dropped after the last job is released by the worker's own
    /// wake, with no later job on its lanes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_lease_is_released_without_another_job() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.db");
        let db = DbWorker::open(&path).await.unwrap();
        let lease = pinned_lease(&db).await;
        assert!(pins(&path) > 0);
        drop(lease);
        crate::test_watchdog::watchdog("released by the worker's own wake", async {
            while pins(&path) > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
    }

    /// A release whose transaction fails keeps its rows and slot; the worker
    /// retries it at its idle maintenance point once storage works again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_release_is_retried_by_the_worker() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.db");
        let db = DbWorker::open(&path).await.unwrap();
        let lease = pinned_lease(&db).await;
        db.call(|db| {
            db.execute_batch("CREATE TRIGGER refuse_release BEFORE DELETE ON runtime_pin BEGIN SELECT RAISE(ABORT,'injected'); END;")?;
            Ok(())
        })
        .await
        .unwrap();
        drop(lease);
        // Any job after the failed attempt: the release is still pending.
        db.call(|_| Ok(())).await.unwrap();
        assert!(pins(&path) > 0, "a failed release keeps its rows");
        db.call(|db| {
            db.execute_batch("DROP TRIGGER refuse_release")?;
            Ok(())
        })
        .await
        .unwrap();
        crate::test_watchdog::watchdog("retried at the idle maintenance point", async {
            while pins(&path) > 0 {
                db.call(|_| Ok(())).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
    }
}
