//! Runtime pins: `runtime_pin` rows that keep a resource out of retention
//! while this owner instance reads or sends it.
//!
//! One [`PinLease`] owns every row whose id starts with its prefix. It is
//! created before the transaction that inserts those rows, so each path that
//! ends the use, including a failure or a caller cancelled after the insert was
//! admitted, releases them through the same lease (#450). A release queued by
//! `Drop` follows the insert on the same DB lane. Rows of an earlier instance
//! are removed at startup.
use crate::maintenance::Work;
use crate::storage::{DbWorker, StorageError};
use rusqlite::Connection;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct PinLease {
    database: DbWorker,
    prefix: String,
    released: AtomicBool,
    /// Retention kinds a release can unblock, marked after it commits.
    unblocks: &'static [Work],
}

impl PinLease {
    pub(crate) fn new(database: DbWorker, unblocks: &'static [Work]) -> Self {
        Self {
            database,
            prefix: uuid::Uuid::new_v4().to_string(),
            released: AtomicBool::new(false),
            unblocks,
        }
    }
    /// Id prefix for the rows this lease owns; [`pin_id`] makes each id.
    pub(crate) fn prefix(&self) -> String {
        self.prefix.clone()
    }
    /// The inserting transaction committed no row, so there is nothing to release.
    pub(crate) fn nothing_inserted(&self) {
        self.released.store(true, Ordering::Release);
    }
    /// Release now and report a failure; `Drop` retries a failed release.
    pub(crate) async fn release(&self) -> Result<(), StorageError> {
        let prefix = self.prefix.clone();
        self.database
            .call(move |db| release_tx(db, &prefix))
            .await?;
        self.released.store(true, Ordering::Release);
        mark(&self.database, self.unblocks);
        Ok(())
    }
}

/// The `n`th row id under a lease prefix.
pub(crate) fn pin_id(prefix: &str, n: usize) -> String {
    format!("{prefix}.{n}")
}

fn release_tx(db: &mut Connection, prefix: &str) -> Result<(), StorageError> {
    db.execute(
        "DELETE FROM runtime_pin WHERE id>?1 AND id<?2",
        [format!("{prefix}."), format!("{prefix}/")],
    )?;
    Ok(())
}

fn mark(database: &DbWorker, unblocks: &[Work]) {
    if !unblocks.is_empty() {
        database.signals().mark_all(unblocks);
    }
}

impl Drop for PinLease {
    fn drop(&mut self) {
        if self.released.load(Ordering::Acquire) {
            return;
        }
        // Without a runtime the owner is stopping; startup cleanup removes
        // this instance's rows next time.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let database = self.database.clone();
            let prefix = std::mem::take(&mut self.prefix);
            let unblocks = self.unblocks;
            runtime.spawn(async move {
                if database
                    .call(move |db| release_tx(db, &prefix))
                    .await
                    .is_ok()
                {
                    mark(&database, unblocks);
                }
            });
        }
    }
}
