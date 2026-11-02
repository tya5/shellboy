//! Coalesced change signals for background maintenance requesters (#171).
//!
//! Producers mark a kind after their commit. The maintenance requester takes
//! the marked set and runs only those kinds until each reports no progress,
//! then waits for the next mark or a data-derived deadline. There is no
//! periodic timer. Startup marks every kind once; an explicit
//! `storage collect` marks every storage kind; and `require_capacity`
//! marks every storage kind when it observes the logical cap, so a missed
//! producer signal cannot let storage grow past the cap unnoticed.
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Work {
    /// Operation receipts: pressure from reservations near the limit.
    Receipts = 1 << 0,
    /// Raw shell output: a terminal execution saved output.
    Raw = 1 << 1,
    /// History reachability: a Thread was deleted.
    History = 1 << 2,
    /// Submitted draft copies: a submission was admitted or cancelled.
    Drafts = 1 << 3,
    /// Summary cache parts whose sources were deleted.
    SummaryCache = 1 << 4,
    /// Index vectors of deleted sources.
    IndexSources = 1 << 5,
    /// Inactive index spaces: a space was deactivated.
    InactiveSpaces = 1 << 6,
    /// Managed clipboard images: an image was saved or released.
    Managed = 1 << 7,
}

impl Work {
    pub const ALL: [Work; 8] = [
        Work::Receipts,
        Work::Raw,
        Work::History,
        Work::Drafts,
        Work::SummaryCache,
        Work::IndexSources,
        Work::InactiveSpaces,
        Work::Managed,
    ];
    /// Source deletion makes derived data eligible as well.
    pub const DELETION: [Work; 3] = [Work::History, Work::SummaryCache, Work::IndexSources];
}

/// The set of kinds taken by one maintenance pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkSet(u32);
impl WorkSet {
    pub fn contains(self, work: Work) -> bool {
        self.0 & work as u32 != 0
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One coalescing bit per kind plus one wake permit. Bounded by `Work`.
#[derive(Debug)]
pub struct Signals {
    dirty: AtomicU32,
    notify: Notify,
    /// Every mark since the last `take_seen`, never consumed by the loop.
    #[cfg(test)]
    seen: AtomicU32,
}

impl Default for Signals {
    /// Startup recomputation: every kind starts marked once.
    fn default() -> Self {
        let signals = Self {
            dirty: AtomicU32::new(0),
            notify: Notify::new(),
            #[cfg(test)]
            seen: AtomicU32::new(0),
        };
        signals.mark_all(&Work::ALL);
        signals
    }
}

impl Signals {
    /// Call only after the producing transaction committed.
    pub fn mark(&self, work: Work) {
        self.mark_all(&[work]);
    }
    pub fn mark_all(&self, works: &[Work]) {
        let bits = works.iter().fold(0, |bits, work| bits | *work as u32);
        #[cfg(test)]
        self.seen.fetch_or(bits, Ordering::AcqRel);
        self.dirty.fetch_or(bits, Ordering::AcqRel);
        self.notify.notify_one();
    }
    /// Producer tests: marks since the previous call, even if a running
    /// maintenance loop already took them.
    #[cfg(test)]
    pub fn take_seen(&self) -> WorkSet {
        WorkSet(self.seen.swap(0, Ordering::AcqRel))
    }
    /// Take every marked kind; later marks are kept for the next pass.
    pub fn take(&self) -> WorkSet {
        WorkSet(self.dirty.swap(0, Ordering::AcqRel))
    }
    /// Put back kinds whose pass ended early (deferred or stopped).
    pub fn restore(&self, set: WorkSet) {
        if !set.is_empty() {
            self.dirty.fetch_or(set.0, Ordering::AcqRel);
        }
    }
    pub fn pending(&self) -> WorkSet {
        WorkSet(self.dirty.load(Ordering::Acquire))
    }
    /// Resolves after a mark; a mark before the wait is not lost (one permit).
    pub async fn changed(&self) {
        self.notify.notified().await;
    }
}

/// Time from now until a SQLite Julian-day instant; zero when already due.
pub fn until_julian(due: f64) -> std::time::Duration {
    let unix = (due - 2_440_587.5) * 86_400.0;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    std::time::Duration::try_from_secs_f64((unix - now).max(0.0)).unwrap_or(std::time::Duration::MAX)
}

/// Time from now until a UTC instant; zero when already due.
pub fn until(due: chrono::DateTime<chrono::Utc>) -> std::time::Duration {
    (due - chrono::Utc::now()).to_std().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn marks_coalesce_and_survive_until_taken() {
        let signals = Signals::default();
        signals.take();
        assert!(signals.take().is_empty());
        signals.mark(Work::Raw);
        signals.mark(Work::Raw);
        signals.mark(Work::Managed);
        // The permit from before the wait is kept.
        signals.changed().await;
        let set = signals.take();
        assert!(set.contains(Work::Raw) && set.contains(Work::Managed));
        assert!(!set.contains(Work::History));
        signals.restore(set);
        assert_eq!(signals.pending(), set);
    }

    #[test]
    fn startup_marks_every_kind_once() {
        let signals = Signals::default();
        let set = signals.take();
        assert!(Work::ALL.iter().all(|w| set.contains(*w)));
    }
}
