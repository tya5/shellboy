//! Runtime pins: `runtime_pin` rows that keep a resource out of retention
//! while this owner instance reads or sends it.
//!
//! One [`PinLease`] owns every row whose id starts with its prefix. The lease
//! is registered as live when it is made, before the job that inserts its
//! rows, and that job holds a clone until it has run (or is dropped unrun), so
//! the last clone ends only after nothing can insert under the prefix any
//! more. Ending it unregisters the prefix and marks [`Work::Pins`]; nothing
//! deletes rows at that point and nothing is spawned or retried.
//!
//! The maintenance owner reconciles: on its wake it deletes the rows whose
//! prefix is not live ([`reconcile_tx`]), a bounded batch at a time. A
//! managed image still `creating` whose pin goes this way is a save whose
//! writer is done without settling it, so the same transaction marks it
//! `failed` for managed collection. Rows of an earlier instance have no live
//! prefix and go the same way after startup.
use crate::maintenance::{Signals, Work};
use crate::storage::DbWorker;
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// The prefixes of this owner's live leases: one per lease that is held by an
/// operation in flight, so it is bounded by those operations.
pub(crate) struct Leases {
    live: Mutex<HashSet<String>>,
    signals: Arc<Signals>,
}

impl Leases {
    pub(crate) fn new(signals: Arc<Signals>) -> Self {
        Self {
            live: Mutex::default(),
            signals,
        }
    }
    fn is_live(&self, prefix: &str) -> bool {
        self.live.lock().expect("pin leases").contains(prefix)
    }
}

/// Cloning shares the lease; the prefix stays live until the last clone drops.
#[derive(Clone)]
pub(crate) struct PinLease(Arc<Live>);

struct Live {
    prefix: String,
    leases: Arc<Leases>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.leases
            .live
            .lock()
            .expect("pin leases")
            .remove(&self.prefix);
        // After unregistering, so the pass this wakes sees the prefix as gone.
        self.leases.signals.mark(Work::Pins);
    }
}

impl PinLease {
    pub(crate) fn new(database: &DbWorker) -> Self {
        let leases = database.pin_leases().clone();
        let prefix = uuid::Uuid::new_v4().to_string();
        leases
            .live
            .lock()
            .expect("pin leases")
            .insert(prefix.clone());
        Self(Arc::new(Live { prefix, leases }))
    }
    /// Id prefix for the rows this lease owns; [`pin_id`] makes each id.
    pub(crate) fn prefix(&self) -> &str {
        &self.0.prefix
    }
}

/// The `n`th row id under a lease prefix.
pub(crate) fn pin_id(prefix: &str, n: usize) -> String {
    format!("{prefix}.{n}")
}

/// One reconciliation unit's outcome.
pub(crate) struct Reconciled {
    /// The last row id visited; the next unit continues after it.
    pub(crate) last: Option<String>,
    /// The unit visited `limit` rows, so more may follow.
    pub(crate) more: bool,
    /// A managed-file or shell-output pin was released.
    pub(crate) managed: bool,
    pub(crate) raw: bool,
}

/// Maintenance owner: visit at most `limit` pin rows after `after` and delete
/// those whose lease is not live. Rows visited, not rows deleted, are bounded.
pub(crate) fn reconcile_tx(
    tx: &Connection,
    leases: &Leases,
    after: &str,
    limit: usize,
) -> rusqlite::Result<Reconciled> {
    let rows = tx
        .prepare_cached(
            "SELECT id,resource_kind,resource_id FROM runtime_pin WHERE id>?1 ORDER BY id LIMIT ?2",
        )?
        .query_map(params![after, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut done = Reconciled {
        last: rows.last().map(|row| row.0.clone()),
        more: rows.len() == limit,
        managed: false,
        raw: false,
    };
    for (id, kind, resource) in &rows {
        let prefix = id
            .rsplit_once('.')
            .map_or(id.as_str(), |(prefix, _)| prefix);
        if leases.is_live(prefix) {
            continue;
        }
        tx.execute("DELETE FROM runtime_pin WHERE id=?1", [id])?;
        match kind.as_str() {
            "managed_file" => {
                tx.execute(
                    "UPDATE managed_file SET state='failed',deleted_reason='creation abandoned' WHERE id=?1 AND state='creating'",
                    [resource],
                )?;
                // A released image pin can make managed images and raw output
                // collectable, as it did when the lease released directly.
                done.managed = true;
                done.raw = true;
            }
            "shell_output" => done.raw = true,
            _ => {}
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(db: &Connection, id: &str, kind: &str, resource: &str) {
        db.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES(?1,'fixture',?2,?3,0,'now')", params![id, kind, resource]).unwrap();
    }

    /// A pin whose lease is gone is deleted by a reconciliation unit, in units
    /// of bounded visits; a live lease's pin and a `creating` image of a live
    /// save stay.
    #[tokio::test]
    async fn reconciliation_deletes_only_pins_of_leases_that_are_not_live() {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("fixture.db"))
            .await
            .unwrap();
        let live = PinLease::new(&db);
        let gone = PinLease::new(&db);
        let (live_id, gone_id) = (pin_id(live.prefix(), 0), pin_id(gone.prefix(), 0));
        let save = pin_id(gone.prefix(), 1);
        db.call(move |db| {
            db.execute("INSERT INTO managed_file(id,absolute_path,state,created_at)VALUES('save','/x/save.png','creating','now')", [])?;
            insert(db, &live_id, "shell_output", "e");
            insert(db, &gone_id, "shell_output", "e");
            insert(db, &save, "managed_file", "save");
            Ok(())
        })
        .await
        .unwrap();
        let leases = db.pin_leases().clone();
        drop(gone);
        let mut after = String::new();
        loop {
            let (leases, cursor) = (leases.clone(), after.clone());
            let done = db
                .call(move |db| Ok(reconcile_tx(db, &leases, &cursor, 1)?))
                .await
                .unwrap();
            let Some(last) = done.last.filter(|_| done.more) else {
                break;
            };
            after = last;
        }
        let (pins, save_state) = db
            .call(|db| {
                Ok((
                    db.query_row("SELECT group_concat(id) FROM runtime_pin", [], |r| {
                        r.get::<_, String>(0)
                    })?,
                    db.query_row("SELECT state FROM managed_file WHERE id='save'", [], |r| {
                        r.get::<_, String>(0)
                    })?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(pins, pin_id(live.prefix(), 0));
        assert_eq!(save_state, "failed");
        db.shutdown().await.unwrap();
    }
}
