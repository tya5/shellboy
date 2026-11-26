//! Runtime pins: `runtime_pin` rows that keep a resource out of retention
//! while this owner instance reads or sends it.
//!
//! One [`PinLease`] owns every row whose id starts with its prefix. It is
//! created before the job that inserts those rows, and that job holds the
//! lease's insert token until it has run (or is dropped unrun). Dropping the
//! lease, on success, failure or cancellation alike, queues one release on the
//! DB worker ([`Releases`]); nothing is spawned and nothing retries from
//! `Drop`. The worker thread drains the queue at every job boundary, in its
//! own transaction, so any job that runs after the drop observes the pins
//! released. A release whose insert job is still pending waits for the
//! boundary after that job; a release whose transaction fails stays queued
//! and the worker retries it at the next boundary. The queue holds at most one
//! entry per lease dropped and not yet released. Rows of an earlier instance
//! are removed at startup.
use crate::maintenance::{Signals, Work};
use crate::storage::DbWorker;
use rusqlite::{Connection, params};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Rows released per drain transaction.
const DRAIN_BATCH: usize = 128;

pub(crate) struct PinLease {
    database: DbWorker,
    prefix: String,
    /// Shared with the inserting job while it can still run.
    insert: Option<Arc<()>>,
    /// A managed image row this lease's save created; still `creating` at
    /// release means the save was abandoned, so the release fails it.
    creating: Option<String>,
    inserted: bool,
}

impl PinLease {
    pub(crate) fn new(database: DbWorker) -> Self {
        Self {
            database,
            prefix: uuid::Uuid::new_v4().to_string(),
            insert: Some(Arc::new(())),
            creating: None,
            inserted: true,
        }
    }
    /// A lease for a clipboard save that inserts `managed` as `creating`.
    pub(crate) fn creating(database: DbWorker, managed: String) -> Self {
        let mut lease = Self::new(database);
        lease.creating = Some(managed);
        lease
    }
    /// Id prefix for the rows this lease owns; [`pin_id`] makes each id.
    pub(crate) fn prefix(&self) -> String {
        self.prefix.clone()
    }
    /// Move this into the inserting job's closure; the release waits for it.
    pub(crate) fn insert_token(&self) -> Arc<()> {
        self.insert.clone().expect("live lease")
    }
    /// The inserting transaction committed no row, so there is nothing to release.
    pub(crate) fn nothing_inserted(&mut self) {
        self.inserted = false;
    }
}

impl Drop for PinLease {
    fn drop(&mut self) {
        if !self.inserted {
            return;
        }
        self.database.pin_releases().push(Release {
            prefix: std::mem::take(&mut self.prefix),
            creating: self.creating.take(),
            insert: self.insert.take().expect("live lease"),
        });
    }
}

/// The `n`th row id under a lease prefix.
pub(crate) fn pin_id(prefix: &str, n: usize) -> String {
    format!("{prefix}.{n}")
}

struct Release {
    prefix: String,
    creating: Option<String>,
    insert: Arc<()>,
}
impl Release {
    /// Its inserting job has run or will never run.
    fn ready(&self) -> bool {
        // The lease moved its handle here; only the queue holds it now.
        Arc::strong_count(&self.insert) == 1
    }
}

/// Releases of dropped leases, owned by the DB worker.
#[derive(Default)]
pub(crate) struct Releases {
    entries: Mutex<Vec<Release>>,
    queued: AtomicUsize,
}

impl Releases {
    fn push(&self, release: Release) {
        let mut entries = self.entries.lock().expect("pin releases");
        entries.push(release);
        self.queued.store(entries.len(), Ordering::Release);
    }
    /// Leases dropped and not yet released.
    pub(crate) fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }
    /// DB worker thread only, between jobs: release up to [`DRAIN_BATCH`]
    /// ready entries in one transaction. A failed transaction keeps them.
    pub(crate) fn drain(&self, db: &mut Connection, signals: &Signals) {
        if self.queued() == 0 {
            return;
        }
        let batch: Vec<Release> = {
            let mut entries = self.entries.lock().expect("pin releases");
            let mut batch = Vec::new();
            let mut index = 0;
            while index < entries.len() && batch.len() < DRAIN_BATCH {
                if entries[index].ready() {
                    batch.push(entries.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            self.queued.store(entries.len(), Ordering::Release);
            batch
        };
        if batch.is_empty() {
            return;
        }
        match release_tx(db, &batch) {
            Ok(managed) => {
                if managed {
                    // Released managed pins and abandoned saves are collectable.
                    signals.mark_all(&[Work::Managed, Work::Raw]);
                }
            }
            Err(_) => {
                let mut entries = self.entries.lock().expect("pin releases");
                entries.extend(batch);
                self.queued.store(entries.len(), Ordering::Release);
            }
        }
    }
}

/// Returns whether any managed-file pin or abandoned save was released.
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
        if let Some(id) = &release.creating {
            managed |= tx.execute(
                "UPDATE managed_file SET state='failed',deleted_reason='save abandoned before completion' WHERE id=?1 AND state='creating'",
                [id],
            )? != 0;
        }
    }
    tx.commit()?;
    Ok(managed)
}
