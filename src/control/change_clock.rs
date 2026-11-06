//! In-memory per-Thread change clock behind `events.wait` (#243).
//!
//! Owner: `Control` (dropped with it). No task, queue, table or DB access.
//! Bound: at most `MAX_CHANGE_THREADS` entries (latest sequence per Thread);
//! older entries are evicted and raise `floor`, so a reader positioned below it
//! is told to resynchronise instead of silently missing changes.
use crate::protocol::MAX_EVENT_CHANGES;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

pub(crate) const MAX_CHANGE_THREADS: usize = 4096;

#[derive(Default)]
struct Inner {
    seq: u64,
    /// Highest sequence ever evicted. A cursor below it may have missed a change.
    floor: u64,
    by_thread: HashMap<String, u64>,
    by_seq: BTreeMap<u64, String>,
}

pub(crate) struct ChangeClock {
    inner: Mutex<Inner>,
    cap: usize,
}

pub(crate) enum Poll {
    /// Cursor older than the retained window.
    Resync { head: u64 },
    Ready {
        changes: Vec<(String, u64)>,
        more: bool,
        /// Position to continue from: last returned change when `more`, else the head.
        cursor: u64,
    },
}

impl ChangeClock {
    pub(crate) fn new() -> Self {
        Self::with_capacity(MAX_CHANGE_THREADS)
    }
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            cap: cap.max(1),
        }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    pub(crate) fn record(&self, thread_id: &str) {
        let mut inner = self.lock();
        inner.seq += 1;
        let seq = inner.seq;
        if let Some(old) = inner.by_thread.insert(thread_id.to_owned(), seq) {
            inner.by_seq.remove(&old);
        }
        inner.by_seq.insert(seq, thread_id.to_owned());
        while inner.by_thread.len() > self.cap {
            let Some((evicted, thread)) = inner.by_seq.pop_first() else {
                break;
            };
            inner.by_thread.remove(&thread);
            inner.floor = evicted;
        }
    }
    pub(crate) fn head(&self) -> u64 {
        self.lock().seq
    }
    /// `None` when `after` is ahead of the clock (a forged or corrupt cursor).
    pub(crate) fn poll(&self, after: u64, filter: &[String]) -> Option<Poll> {
        let inner = self.lock();
        if after > inner.seq {
            return None;
        }
        if after < inner.floor {
            return Some(Poll::Resync { head: inner.seq });
        }
        let mut changes = Vec::new();
        let mut more = false;
        for (seq, thread) in inner.by_seq.range(after + 1..) {
            if !filter.is_empty() && !filter.iter().any(|wanted| wanted == thread) {
                continue;
            }
            if changes.len() == MAX_EVENT_CHANGES {
                more = true;
                break;
            }
            changes.push((thread.clone(), *seq));
        }
        let cursor = if more {
            changes.last().map_or(inner.seq, |(_, seq)| *seq)
        } else {
            inner.seq
        };
        Some(Poll::Ready {
            changes,
            more,
            cursor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(poll: Option<Poll>) -> (Vec<(String, u64)>, bool, u64) {
        match poll {
            Some(Poll::Ready {
                changes,
                more,
                cursor,
            }) => (changes, more, cursor),
            _ => panic!("expected ready"),
        }
    }

    #[test]
    fn orders_by_sequence_and_merges_repeated_changes() {
        let clock = ChangeClock::new();
        for id in ["a", "b", "a", "a", "c"] {
            clock.record(id);
        }
        let (changes, more, cursor) = ready(clock.poll(0, &[]));
        assert_eq!(
            changes,
            vec![("b".into(), 2), ("a".into(), 4), ("c".into(), 5)]
        );
        assert!(!more);
        assert_eq!(cursor, 5);
        let (changes, _, _) = ready(clock.poll(2, &[]));
        assert_eq!(changes.len(), 2);
        let (none, _, cursor) = ready(clock.poll(5, &[]));
        assert!(none.is_empty());
        assert_eq!(cursor, 5);
    }

    #[test]
    fn filter_limits_changes_but_cursor_still_reaches_head() {
        let clock = ChangeClock::new();
        clock.record("a");
        clock.record("b");
        let (changes, _, cursor) = ready(clock.poll(0, &["b".into()]));
        assert_eq!(changes, vec![("b".into(), 2)]);
        assert_eq!(cursor, 2);
        let (changes, _, cursor) = ready(clock.poll(0, &["zzz".into()]));
        assert!(changes.is_empty());
        assert_eq!(cursor, 2);
    }

    #[test]
    fn page_of_sixty_four_reports_more_and_resumes_without_a_gap() {
        let clock = ChangeClock::new();
        for i in 0..100 {
            clock.record(&format!("t{i}"));
        }
        let (first, more, cursor) = ready(clock.poll(0, &[]));
        assert_eq!(first.len(), MAX_EVENT_CHANGES);
        assert!(more);
        assert_eq!(cursor, 64);
        let (rest, more, cursor) = ready(clock.poll(cursor, &[]));
        assert_eq!(rest.len(), 36);
        assert!(!more);
        assert_eq!(cursor, 100);
        assert_eq!(rest[0].0, "t64");
    }

    #[test]
    fn eviction_raises_floor_and_demands_resync_only_below_it() {
        let clock = ChangeClock::with_capacity(3);
        for id in ["a", "b", "c", "d"] {
            clock.record(id);
        }
        // "a" (seq 1) was evicted: a reader at 0 may have missed it.
        assert!(matches!(clock.poll(0, &[]), Some(Poll::Resync { head: 4 })));
        // A reader at the floor has already seen everything evicted.
        let (changes, _, _) = ready(clock.poll(1, &[]));
        assert_eq!(changes.len(), 3);
        assert!(clock.lock().by_thread.len() <= 3);
    }

    #[test]
    fn repeated_changes_to_one_thread_never_evict() {
        let clock = ChangeClock::with_capacity(2);
        for _ in 0..10_000 {
            clock.record("a");
        }
        clock.record("b");
        let (changes, _, _) = ready(clock.poll(0, &[]));
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn cursor_ahead_of_the_clock_is_rejected() {
        let clock = ChangeClock::new();
        clock.record("a");
        assert!(clock.poll(2, &[]).is_none());
    }
}
