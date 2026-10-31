//! At most two visible count scopes on existing frontend read admission.
use super::lineage::{self, DescendantSnapshot, LineageScope, RawLineage};
use crate::{
    protocol::{ApiError, ErrorCode},
    storage::StorageError,
    ui::{UiUpdate, watch::DescendantCount},
};
use rusqlite::Connection;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const PERIOD: Duration = Duration::from_millis(500);
#[derive(Clone)]
pub(super) struct Ticket {
    pub generation: u64,
    pub roots: Vec<String>,
    pub stop: CancellationToken,
    serial: u64,
    force: bool,
    changed: Vec<String>,
    cached: Vec<DescendantSnapshot>,
}
pub(super) struct RawBatch {
    cached: Vec<DescendantSnapshot>,
    rows: Vec<RawLineage>,
    pub elapsed: Duration,
    queried_roots: usize,
}
impl RawBatch {
    /// Saved scalar membership only, without classification or JSON parsing.
    pub fn scope(&self, root: &str) -> Option<(bool, Vec<String>)> {
        self.rows
            .iter()
            .find(|row| row.root() == root)
            .map(lineage::membership)
            .or_else(|| {
                self.cached
                    .iter()
                    .find(|row| row.root == root)
                    .map(|row| (row.available, row.threads.clone()))
            })
    }
    pub fn prepare(self) -> Prepared {
        let discovered = self.rows.iter().map(RawLineage::witnesses).sum();
        let mut truncated = false;
        let mut snapshots = self
            .cached
            .into_iter()
            .map(|row| (row.root.clone(), row))
            .collect::<BTreeMap<_, _>>();
        for row in self.rows {
            let row = lineage::prepare(row);
            truncated |= row.truncated;
            snapshots.insert(row.root.clone(), row);
        }
        Prepared {
            snapshots: snapshots.into_values().collect(),
            elapsed: self.elapsed,
            queried_roots: self.queried_roots,
            discovered,
            truncated,
        }
    }
}
pub(super) struct Prepared {
    pub snapshots: Vec<DescendantSnapshot>,
    pub elapsed: Duration,
    pub queried_roots: usize,
    pub discovered: usize,
    pub truncated: bool,
}
pub(super) fn read_tx(db: &Connection, ticket: &Ticket) -> Result<RawBatch, StorageError> {
    if ticket.stop.is_cancelled() {
        return Err(ApiError::new(ErrorCode::InvalidState, "Descendant observation closed").into());
    }
    let began = std::time::Instant::now();
    let scopes = ticket
        .cached
        .iter()
        .map(|row| LineageScope {
            root: row.root.clone(),
            members: row.members.clone(),
        })
        .collect::<Vec<_>>();
    let roots = if ticket.force {
        ticket.roots.clone()
    } else {
        lineage::affected_tx(db, &ticket.changed, &scopes)?
    };
    let rows = lineage::read_roots_tx(db, &roots)?;
    Ok(RawBatch {
        cached: ticket.cached.clone(),
        rows,
        elapsed: began.elapsed(),
        queried_roots: roots.len(),
    })
}
#[derive(Default)]
pub(super) struct Observer {
    generation: Option<u64>,
    roots: Vec<String>,
    stop: CancellationToken,
    changed: BTreeSet<String>,
    force: bool,
    next: Option<Instant>,
    serial: u64,
    inflight: Option<u64>,
    cached: BTreeMap<String, DescendantSnapshot>,
    last_sequence: u64,
    error: Option<String>,
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Observer {
    pub fn snapshots(&self) -> impl Iterator<Item = &DescendantSnapshot> {
        self.cached.values()
    }
    pub fn observe(&mut self, generation: u64, mut roots: Vec<String>) -> Result<bool, ApiError> {
        if self.generation.is_some_and(|old| generation < old) {
            return Ok(false);
        }
        if roots.len() > 2 || roots.iter().any(|id| id.is_empty() || id.len() > 256) {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Descendant observation exceeds its two-root identity bound",
            ));
        }
        roots.sort();
        roots.dedup();
        if self.generation == Some(generation) {
            if self.roots != roots {
                return Err(ApiError::new(
                    ErrorCode::InvalidState,
                    "Descendant roots changed without a new scope generation",
                ));
            }
            return Ok(false);
        }
        self.stop.cancel();
        self.stop = CancellationToken::new();
        self.generation = Some(generation);
        self.roots = roots;
        self.changed.clear();
        self.inflight = None;
        self.force = !self.roots.is_empty();
        self.cached.retain(|root, _| self.roots.contains(root));
        self.last_sequence = 0;
        self.error = None;
        Ok(true)
    }
    pub fn changed(&mut self, id: &str) {
        if self.roots.is_empty() || self.force {
            return;
        }
        if id.is_empty()
            || id.len() > 256
            || self.changed.len() >= 128 && !self.changed.contains(id)
        {
            self.resync();
            return;
        }
        self.changed.insert(id.to_owned());
    }
    pub fn resync(&mut self) {
        if !self.roots.is_empty() {
            self.force = true;
            self.changed.clear();
        }
    }
    pub fn deadline(&self) -> Option<Instant> {
        (!self.roots.is_empty()
            && self.inflight.is_none()
            && (self.force || !self.changed.is_empty()))
        .then(|| self.next.unwrap_or_else(Instant::now))
    }
    pub fn take(&mut self, now: Instant) -> Option<Ticket> {
        if self.roots.is_empty()
            || self.inflight.is_some()
            || (!self.force && self.changed.is_empty())
            || self.next.is_some_and(|due| due > now)
        {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        self.inflight = Some(self.serial);
        self.next = Some(now + PERIOD);
        let force = std::mem::take(&mut self.force);
        let changed = std::mem::take(&mut self.changed).into_iter().collect();
        Some(Ticket {
            generation: self.generation.expect("visible scope has generation"),
            roots: self.roots.clone(),
            stop: self.stop.child_token(),
            serial: self.serial,
            force,
            changed,
            cached: self.cached.values().cloned().collect(),
        })
    }
    /// Returns membership deltas independently of whether visible count text changed.
    pub fn complete(
        &mut self,
        ticket: &Ticket,
        sequence: u64,
        result: Result<Prepared, ApiError>,
    ) -> Option<Completion> {
        if ticket.stop.is_cancelled()
            || self.generation != Some(ticket.generation)
            || self.inflight != Some(ticket.serial)
        {
            return None;
        }
        self.inflight = None;
        if sequence < self.last_sequence {
            self.resync();
            return None;
        }
        let mut changed_members = BTreeSet::new();
        let mut counts_changed = self.cached.is_empty();
        let error = match result {
            Ok(prepared) => {
                for row in prepared.snapshots {
                    if !self.roots.contains(&row.root) {
                        continue;
                    }
                    if let Some(old) = self.cached.get(&row.root) {
                        counts_changed |= count(old) != count(&row);
                        let old = old.threads.iter().collect::<BTreeSet<_>>();
                        let new = row.threads.iter().collect::<BTreeSet<_>>();
                        changed_members
                            .extend(old.symmetric_difference(&new).map(|id| (*id).clone()));
                    } else {
                        changed_members.extend(row.threads.iter().cloned());
                        counts_changed = true;
                    }
                    self.cached.insert(row.root.clone(), row);
                }
                None
            }
            Err(error) => {
                self.resync();
                Some(error.to_string())
            }
        };
        counts_changed |= self.error != error;
        self.error = error.clone();
        self.last_sequence = sequence;
        let update = counts_changed.then(|| UiUpdate::Descendants {
            generation: ticket.generation,
            read_sequence: sequence,
            counts: self
                .roots
                .iter()
                .map(|root| {
                    self.cached
                        .get(root)
                        .map(count)
                        .unwrap_or_else(|| DescendantCount {
                            root: root.clone(),
                            available: false,
                            active: 0,
                            recovery: 0,
                            truncated: false,
                            cycle: false,
                        })
                })
                .collect(),
            error,
        });
        Some(Completion {
            update,
            changed_members: changed_members.into_iter().collect(),
        })
    }
}
pub(super) struct Completion {
    pub update: Option<UiUpdate>,
    pub changed_members: Vec<String>,
}
fn count(row: &DescendantSnapshot) -> DescendantCount {
    DescendantCount {
        root: row.root.clone(),
        available: row.available,
        active: row.active,
        recovery: row.recovery,
        truncated: row.truncated,
        cycle: row.cycle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(root: &str, ids: &[&str]) -> DescendantSnapshot {
        DescendantSnapshot {
            root: root.into(),
            available: true,
            active: 1,
            recovery: 0,
            truncated: false,
            cycle: false,
            threads: ids.iter().map(|id| (*id).into()).collect(),
            members: ids.iter().map(|id| (*id).into()).collect(),
        }
    }
    fn prepared(row: DescendantSnapshot) -> Prepared {
        Prepared {
            snapshots: vec![row],
            elapsed: Duration::ZERO,
            queried_roots: 1,
            discovered: 0,
            truncated: false,
        }
    }
    #[test]
    fn hidden_scopes_do_no_work_and_scope_change_retires_inflight_without_resetting_deadline() {
        let mut observer = Observer::default();
        observer.changed("unrelated");
        assert!(observer.deadline().is_none());
        let now = Instant::now();
        observer.observe(1, vec!["root".into()]).unwrap();
        let first = observer.take(now).unwrap();
        assert!(observer.take(now).is_none());
        observer.observe(2, vec![]).unwrap();
        assert!(first.stop.is_cancelled());
        assert!(observer.deadline().is_none());
        assert!(
            observer
                .complete(&first, 1, Ok(prepared(snapshot("root", &[]))))
                .is_none()
        );
        observer.observe(3, vec!["other".into()]).unwrap();
        assert!(observer.take(now).is_none());
        assert!(observer.take(now + PERIOD).is_some());
        assert!(!observer.observe(2, vec!["old".into()]).unwrap());
        assert!(observer.observe(3, vec!["wrong".into()]).is_err());
    }
    #[test]
    fn notifications_during_a_read_survive_and_unchanged_counts_still_return_membership_deltas() {
        let now = Instant::now();
        let mut observer = Observer::default();
        observer.observe(1, vec!["root".into()]).unwrap();
        let first = observer.take(now).unwrap();
        observer.changed("late-child");
        let initial = observer
            .complete(&first, 1, Ok(prepared(snapshot("root", &["child"]))))
            .unwrap();
        assert!(initial.update.is_some());
        assert!(observer.deadline().is_some());
        let second = observer.take(now + PERIOD).unwrap();
        assert_eq!(second.changed, ["late-child"]);
        let next = observer
            .complete(
                &second,
                2,
                Ok(prepared(snapshot("root", &["child", "new-child"]))),
            )
            .unwrap();
        assert!(next.update.is_none());
        assert_eq!(next.changed_members, ["new-child"]);
        assert!(observer.deadline().is_none());
    }
    #[test]
    fn overflow_and_failed_reads_use_one_bounded_resynchronization_without_faking_zero() {
        let now = Instant::now();
        let mut observer = Observer::default();
        observer.observe(1, vec!["root".into()]).unwrap();
        let initial = observer.take(now).unwrap();
        observer.complete(&initial, 1, Ok(prepared(snapshot("root", &["child"]))));
        for n in 0..129 {
            observer.changed(&format!("c{n}"));
        }
        assert!(observer.force && observer.changed.is_empty());
        let failed = observer.take(now + PERIOD).unwrap();
        let completion = observer
            .complete(
                &failed,
                2,
                Err(ApiError::new(ErrorCode::InvalidState, "busy")),
            )
            .unwrap();
        assert!(
            matches!(completion.update,Some(UiUpdate::Descendants{counts,error:Some(_),..}) if counts[0].active==1)
        );
        assert!(observer.force);
        assert!(observer.take(now + PERIOD).is_none());
        assert!(observer.take(now + PERIOD * 2).is_some());
    }
}
