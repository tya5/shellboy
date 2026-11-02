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
    /// Ended Runs whose own deliveries and shell events are retained (#170):
    /// processed per Run through indexed lookups, from `Signals::run_ended`.
    EndedRuns = 1 << 8,
    /// The full event-retention sweep for rows that predate this owner:
    /// startup, explicit `storage collect`, or an ended-Run queue overflow.
    EventBacklog = 1 << 9,
}

impl Work {
    pub const ALL: [Work; 10] = [
        Work::Receipts,
        Work::Raw,
        Work::History,
        Work::Drafts,
        Work::SummaryCache,
        Work::IndexSources,
        Work::InactiveSpaces,
        Work::Managed,
        Work::EndedRuns,
        Work::EventBacklog,
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
    pub fn insert(&mut self, work: Work) {
        self.0 |= work as u32;
    }
}

/// Ended Runs remembered for targeted retention. Beyond this, a full
/// backlog sweep replaces the per-Run work (bounded memory, no loss).
pub const ENDED_RUNS_CAPACITY: usize = 1024;

/// One coalescing bit per kind plus one wake permit, and the bounded queue of
/// ended Run ids. Bounded by `Work` and `ENDED_RUNS_CAPACITY`.
#[derive(Debug)]
pub struct Signals {
    dirty: AtomicU32,
    notify: Notify,
    ended_runs: std::sync::Mutex<std::collections::VecDeque<String>>,
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
            ended_runs: std::sync::Mutex::default(),
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
    /// Call after a Run's terminal state committed: its own deliveries and
    /// shell events become eligible for targeted retention.
    pub fn run_ended(&self, run: &str) {
        let overflow = {
            let mut runs = self.ended_runs.lock().expect("ended runs");
            if runs.iter().any(|known| known == run) {
                false
            } else if runs.len() < ENDED_RUNS_CAPACITY {
                runs.push_back(run.to_owned());
                false
            } else {
                true
            }
        };
        self.mark(if overflow {
            Work::EventBacklog
        } else {
            Work::EndedRuns
        });
    }
    /// Ended Runs to process now; the caller re-queues the unfinished ones.
    pub fn take_ended_runs(&self) -> Vec<String> {
        self.ended_runs
            .lock()
            .expect("ended runs")
            .drain(..)
            .collect()
    }
    /// Re-queue Runs whose retention was deferred, ahead of newer ones.
    pub fn requeue_ended_runs(&self, runs: Vec<String>) {
        if runs.is_empty() {
            return;
        }
        let overflow = {
            let mut queue = self.ended_runs.lock().expect("ended runs");
            for run in runs.into_iter().rev() {
                if !queue.contains(&run) {
                    queue.push_front(run);
                }
            }
            let overflow = queue.len() > ENDED_RUNS_CAPACITY;
            queue.truncate(ENDED_RUNS_CAPACITY);
            overflow
        };
        self.mark(Work::EndedRuns);
        if overflow {
            self.mark(Work::EventBacklog);
        }
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
    std::time::Duration::try_from_secs_f64((unix - now).max(0.0))
        .unwrap_or(std::time::Duration::MAX)
}

/// Time from now until a UTC instant; zero when already due.
pub fn until(due: chrono::DateTime<chrono::Utc>) -> std::time::Duration {
    (due - chrono::Utc::now()).to_std().unwrap_or_default()
}

#[cfg(test)]
mod step_tests;

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
    fn ended_runs_are_bounded_and_overflow_falls_back_to_the_backlog_sweep() {
        let signals = Signals::default();
        signals.take();
        for n in 0..ENDED_RUNS_CAPACITY {
            signals.run_ended(&format!("r{n}"));
        }
        signals.run_ended("r0");
        assert_eq!(signals.take(), {
            let mut set = WorkSet::default();
            set.insert(Work::EndedRuns);
            set
        });
        signals.run_ended("overflow");
        assert!(signals.take().contains(Work::EventBacklog));
        let runs = signals.take_ended_runs();
        assert_eq!(runs.len(), ENDED_RUNS_CAPACITY);
        assert!(signals.take_ended_runs().is_empty());
    }

    #[test]
    fn startup_marks_every_kind_once() {
        let signals = Signals::default();
        let set = signals.take();
        assert!(Work::ALL.iter().all(|w| set.contains(*w)));
    }
}
