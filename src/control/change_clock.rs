//! In-memory per-Thread change clock behind `events.wait` (#243).
//!
//! Owner: `Control` (dropped with it). No task, queue, table or DB access.
//! Bound: at most `MAX_CHANGE_THREADS` entries (latest sequence per Thread);
//! older entries are evicted and raise `floor`, so a reader positioned below it
//! is told to resynchronise instead of silently missing changes.
//!
//! The same lock and sequence also carry unsaved model text and attempt-scoped
//! reasoning activity for clients that opt in (#243 E3, #275). Both are freed when
//! the attempt ends (`LiveTextGuard`), never saved and never a source of truth.
//! Writers never wait
//! for a reader: the lock is held only for memory updates and copies, never across
//! `.await` or I/O, and a reader that stops reading holds nothing here.
use crate::protocol::{
    EventLiveText, EventReasoningActivity, EventTextDelta, MAX_EVENT_CHANGES, MAX_FRAME_BYTES,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Mutex,
};
use tokio::sync::Notify;

pub(crate) const MAX_CHANGE_THREADS: usize = 4096;
/// Streaming attempts holding text at once; further attempts are refused until one ends.
pub(crate) const MAX_LIVE_TEXT_ATTEMPTS: usize = 16;
/// Retained text per attempt, each delta also charged its record size. Same size as
/// the TUI's per-attempt preview window (tui-presentation.md).
pub(crate) const MAX_ATTEMPT_TEXT_BYTES: usize = 64 * 1024;
/// JSON writes a control character as `\u00XX`, the worst expansion of retained bytes.
const JSON_WORST_EXPANSION: usize = 6;
// All retained text always fits one reply, so text needs no paging of its own. The
// margin covers the envelope, IDs and the 64 change entries.
const _: () = assert!(
    MAX_LIVE_TEXT_ATTEMPTS * MAX_ATTEMPT_TEXT_BYTES * JSON_WORST_EXPANSION + 1024 * 1024
        <= MAX_FRAME_BYTES
);

struct Delta {
    seq: u64,
    offset: u64,
    reasoning: bool,
    text: Box<str>,
}
const DELTA_RECORD_BYTES: usize = std::mem::size_of::<Delta>();

struct AttemptText {
    attempt: String,
    /// Ascending by sequence; never empty after the first write.
    deltas: VecDeque<Delta>,
    charged: usize,
    /// Highest sequence whose text is gone. A reader below it has missed text.
    floor: u64,
}

#[derive(Default)]
struct Inner {
    seq: u64,
    /// Highest sequence ever evicted. A cursor below it may have missed a change.
    floor: u64,
    by_thread: HashMap<String, u64>,
    by_seq: BTreeMap<u64, String>,
    /// Keyed by Thread: a Thread streams one attempt at a time, and a later attempt replaces it.
    text: HashMap<String, AttemptText>,
    reasoning_activity: HashMap<String, AttemptActivity>,
}

struct AttemptActivity {
    token: String,
    attempt: String,
    started: std::time::Instant,
    received_bytes: Option<u64>,
    sequence: u64,
}

pub(crate) struct ChangeClock {
    inner: Mutex<Inner>,
    cap: usize,
}

pub(crate) enum Poll {
    /// Cursor older than the retained window.
    Resync {
        head: u64,
        text: Vec<EventLiveText>,
        reasoning_activity: Vec<EventReasoningActivity>,
    },
    Ready {
        changes: Vec<(String, u64)>,
        more: bool,
        /// Position to continue from: last returned change when `more`, else the head.
        cursor: u64,
        /// Live attempts and their text up to `cursor`; empty unless asked for.
        text: Vec<EventLiveText>,
        reasoning_activity: Vec<EventReasoningActivity>,
        reasoning_changed: bool,
    },
}

/// Frees an attempt's retained text however its stream ends (return, cancel, drop).
pub(crate) struct LiveTextGuard<'a> {
    clock: &'a ChangeClock,
    thread: &'a str,
    attempt: &'a str,
    changed: &'a Notify,
    token: String,
    alive: std::sync::Arc<Mutex<bool>>,
}
impl LiveTextGuard<'_> {
    pub(crate) fn reservation(&self) -> (String, std::sync::Arc<Mutex<bool>>) {
        (self.token.clone(), self.alive.clone())
    }
    pub(crate) fn record(&self, offset: u64, text: &str, reasoning: bool) -> bool {
        self.clock.record_admitted_text(
            self.thread,
            self.attempt,
            &self.token,
            offset,
            text,
            reasoning,
        )
    }
}
impl Drop for LiveTextGuard<'_> {
    fn drop(&mut self) {
        let mut alive = self.alive.lock().unwrap_or_else(|p| p.into_inner());
        *alive = false;
        self.clock
            .revoke_live(self.thread, self.attempt, Some(&self.token));
        self.changed.notify_waiters();
    }
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
    pub(crate) fn reserve_live<'a>(
        &'a self,
        thread: &'a str,
        attempt: &'a str,
        changed: &'a Notify,
    ) -> LiveTextGuard<'a> {
        LiveTextGuard {
            clock: self,
            thread,
            attempt,
            changed,
            token: uuid::Uuid::new_v4().to_string(),
            alive: std::sync::Arc::new(Mutex::new(true)),
        }
    }
    // Worker -> reservation -> bounded memory; drop uses the same final two locks.
    pub(crate) fn admit_live(&self, thread: &str, attempt: &str, token: &str, alive: &Mutex<bool>) {
        let live = alive.lock().unwrap_or_else(|p| p.into_inner());
        if !*live {
            return;
        }
        let mut inner = self.lock();
        if inner.reasoning_activity.len() >= MAX_LIVE_TEXT_ATTEMPTS
            && !inner.reasoning_activity.contains_key(thread)
        {
            return;
        }
        inner.seq += 1;
        let sequence = inner.seq;
        inner.text.remove(thread);
        inner.reasoning_activity.insert(
            thread.into(),
            AttemptActivity {
                token: token.into(),
                attempt: attempt.into(),
                started: std::time::Instant::now(),
                received_bytes: None,
                sequence,
            },
        );
        inner.text.insert(
            thread.into(),
            AttemptText {
                attempt: attempt.into(),
                deltas: VecDeque::new(),
                charged: 0,
                floor: 0,
            },
        );
    }
    pub(crate) fn revoke_live(&self, thread: &str, attempt: &str, token: Option<&str>) {
        let mut inner = self.lock();
        let matches = inner
            .reasoning_activity
            .get(thread)
            .is_some_and(|a| a.attempt == attempt && token.is_none_or(|t| a.token == t));
        if !matches {
            return;
        }
        inner.reasoning_activity.remove(thread);
        inner.text.remove(thread);
        drop(inner);
        self.record(thread);
    }
    #[cfg(test)]
    pub(crate) fn live_text_guard<'a>(
        &'a self,
        thread: &'a str,
        attempt: &'a str,
        changed: &'a Notify,
    ) -> LiveTextGuard<'a> {
        let guard = self.reserve_live(thread, attempt, changed);
        self.admit_live(thread, attempt, &guard.token, &guard.alive);
        changed.notify_waiters();
        guard
    }
    pub(crate) fn record_admitted_text(
        &self,
        thread: &str,
        attempt: &str,
        token: &str,
        offset: u64,
        text: &str,
        reasoning: bool,
    ) -> bool {
        self.record_text_inner(thread, attempt, Some(token), offset, text, reasoning)
    }
    #[cfg(test)]
    pub(crate) fn record_text(
        &self,
        thread: &str,
        attempt: &str,
        offset: u64,
        text: &str,
        reasoning: bool,
    ) {
        // Legacy buffer-size tests inject unqualified text; production never does.
        self.record_text_inner(thread, attempt, None, offset, text, reasoning);
    }
    /// `offset` counts the Unicode scalar values already streamed by this attempt.
    fn record_text_inner(
        &self,
        thread: &str,
        attempt: &str,
        token: Option<&str>,
        offset: u64,
        text: &str,
        reasoning: bool,
    ) -> bool {
        if text.is_empty() {
            return false;
        }
        let mut inner = self.lock();
        if let Some(token) = token
            && (!inner
                .reasoning_activity
                .get(thread)
                .is_some_and(|a| a.attempt == attempt && a.token == token)
                || !inner.text.get(thread).is_some_and(|a| a.attempt == attempt))
        {
            return false;
        }
        // Both guarded and tokenless updates share the active-thread bound.
        if !inner.text.contains_key(thread) && inner.text.len() >= MAX_LIVE_TEXT_ATTEMPTS {
            return false;
        }
        inner.seq += 1;
        let seq = inner.seq;
        if inner
            .text
            .get(thread)
            .is_none_or(|current| current.attempt != attempt)
        {
            inner.text.insert(
                thread.to_owned(),
                AttemptText {
                    attempt: attempt.to_owned(),
                    deltas: VecDeque::new(),
                    charged: 0,
                    // Entered mid-attempt after being refused: earlier text is gone.
                    floor: if offset == 0 { 0 } else { seq },
                },
            );
        }
        // A single oversized delta keeps its tail, cut on a character boundary.
        let room = MAX_ATTEMPT_TEXT_BYTES - DELTA_RECORD_BYTES;
        let start = text.ceil_char_boundary(text.len().saturating_sub(room));
        let offset = offset + text[..start].chars().count() as u64;
        let kept = &text[start..];
        if reasoning
            && let Some(activity) = inner
                .reasoning_activity
                .get_mut(thread)
                .filter(|activity| activity.attempt == attempt)
        {
            let bytes = activity.received_bytes.get_or_insert(0);
            *bytes = bytes.saturating_add(text.len() as u64);
            activity.sequence = seq;
        }
        let entry = inner.text.get_mut(thread).expect("entry inserted above");
        if start > 0 {
            entry.floor = seq;
        }
        entry.charged += DELTA_RECORD_BYTES + kept.len();
        entry.deltas.push_back(Delta {
            seq,
            offset,
            reasoning,
            text: kept.into(),
        });
        while entry.charged > MAX_ATTEMPT_TEXT_BYTES {
            let Some(oldest) = entry.deltas.pop_front() else {
                break;
            };
            entry.charged -= DELTA_RECORD_BYTES + oldest.text.len();
            entry.floor = entry.floor.max(oldest.seq);
        }
        true
    }
    /// The head and, when asked for, everything retained: for a client with no position.
    pub(crate) fn snapshot(
        &self,
        filter: &[String],
        include_text: bool,
        include_reasoning_activity: bool,
    ) -> (u64, Vec<EventLiveText>, Vec<EventReasoningActivity>) {
        let inner = self.lock();
        let text = if include_text {
            inner.live_text(0, inner.seq, filter)
        } else {
            Vec::new()
        };
        let activity = if include_reasoning_activity {
            inner.reasoning_activity_since(filter, 0, inner.seq).0
        } else {
            Vec::new()
        };
        (inner.seq, text, activity)
    }
    /// `None` when `after` is ahead of the clock (a forged or corrupt cursor).
    pub(crate) fn poll(
        &self,
        after: u64,
        filter: &[String],
        include_text: bool,
        include_reasoning_activity: bool,
    ) -> Option<Poll> {
        let inner = self.lock();
        if after > inner.seq {
            return None;
        }
        if after < inner.floor {
            let text = if include_text {
                inner.live_text(0, inner.seq, filter)
            } else {
                Vec::new()
            };
            let reasoning_activity = if include_reasoning_activity {
                inner.reasoning_activity_since(filter, after, inner.seq).0
            } else {
                Vec::new()
            };
            return Some(Poll::Resync {
                head: inner.seq,
                text,
                reasoning_activity,
            });
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
        let text = if include_text {
            inner.live_text(after, cursor, filter)
        } else {
            Vec::new()
        };
        let (reasoning_activity, reasoning_changed) = if include_reasoning_activity {
            inner.reasoning_activity_since(filter, after, cursor)
        } else {
            (Vec::new(), false)
        };
        Some(Poll::Ready {
            changes,
            more,
            cursor,
            text,
            reasoning_activity,
            reasoning_changed,
        })
    }
}

impl Inner {
    fn reasoning_activity_since(
        &self,
        filter: &[String],
        after: u64,
        upto: u64,
    ) -> (Vec<EventReasoningActivity>, bool) {
        let now = std::time::Instant::now();
        let changed = self.reasoning_activity.iter().any(|(thread, activity)| {
            (filter.is_empty() || filter.iter().any(|wanted| wanted == thread))
                && activity.sequence > after
                && activity.sequence <= upto
        });
        let mut items: Vec<_> = self
            .reasoning_activity
            .iter()
            .filter(|(thread, activity)| {
                (filter.is_empty() || filter.iter().any(|wanted| wanted == *thread))
                    && activity.sequence <= upto
            })
            .map(|(thread, activity)| EventReasoningActivity {
                thread_id: thread.clone(),
                attempt_id: activity.attempt.clone(),
                elapsed_ms: now
                    .saturating_duration_since(activity.started)
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
                received_bytes: activity.received_bytes,
            })
            .collect();
        items.sort_by(|a, b| a.thread_id.cmp(&b.thread_id));
        (items, changed)
    }

    /// Every live attempt in the filter with its text in `(after, upto]`. Adjacent
    /// deltas of one kind merge; a listed attempt with no text is still live.
    fn live_text(&self, after: u64, upto: u64, filter: &[String]) -> Vec<EventLiveText> {
        let mut attempts = Vec::new();
        for (thread, entry) in &self.text {
            // Admission reserves capacity, but is not itself a received text delta.
            // Retired deltas retain a floor, so their empty/gap entries stay visible.
            if entry.deltas.is_empty() && entry.floor == 0 {
                continue;
            }

            if !filter.is_empty() && !filter.iter().any(|wanted| wanted == thread) {
                continue;
            }
            let mut deltas: Vec<EventTextDelta> = Vec::new();
            let start = entry.deltas.partition_point(|delta| delta.seq <= after);
            for delta in entry.deltas.range(start..) {
                if delta.seq > upto {
                    break;
                }
                if delta.reasoning {
                    continue;
                }
                match deltas.last_mut() {
                    Some(last) if last.reasoning == delta.reasoning => {
                        last.text.push_str(&delta.text);
                    }
                    _ => deltas.push(EventTextDelta {
                        offset: delta.offset,
                        reasoning: delta.reasoning,
                        text: delta.text.as_ref().to_owned(),
                    }),
                }
            }
            attempts.push(EventLiveText {
                thread_id: thread.clone(),
                attempt_id: entry.attempt.clone(),
                gap: after < entry.floor,
                deltas,
            });
        }
        attempts.sort_by(|a, b| a.thread_id.cmp(&b.thread_id));
        attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn ready(poll: Option<Poll>) -> (Vec<(String, u64)>, bool, u64) {
        match poll {
            Some(Poll::Ready {
                changes,
                more,
                cursor,
                ..
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
        let (changes, more, cursor) = ready(clock.poll(0, &[], false, false));
        assert_eq!(
            changes,
            vec![("b".into(), 2), ("a".into(), 4), ("c".into(), 5)]
        );
        assert!(!more);
        assert_eq!(cursor, 5);
        let (changes, _, _) = ready(clock.poll(2, &[], false, false));
        assert_eq!(changes.len(), 2);
        let (none, _, cursor) = ready(clock.poll(5, &[], false, false));
        assert!(none.is_empty());
        assert_eq!(cursor, 5);
    }

    #[test]
    fn filter_limits_changes_but_cursor_still_reaches_head() {
        let clock = ChangeClock::new();
        clock.record("a");
        clock.record("b");
        let (changes, _, cursor) = ready(clock.poll(0, &["b".into()], false, false));
        assert_eq!(changes, vec![("b".into(), 2)]);
        assert_eq!(cursor, 2);
        let (changes, _, cursor) = ready(clock.poll(0, &["zzz".into()], false, false));
        assert!(changes.is_empty());
        assert_eq!(cursor, 2);
    }

    #[test]
    fn page_of_sixty_four_reports_more_and_resumes_without_a_gap() {
        let clock = ChangeClock::new();
        for i in 0..100 {
            clock.record(&format!("t{i}"));
        }
        let (first, more, cursor) = ready(clock.poll(0, &[], false, false));
        assert_eq!(first.len(), MAX_EVENT_CHANGES);
        assert!(more);
        assert_eq!(cursor, 64);
        let (rest, more, cursor) = ready(clock.poll(cursor, &[], false, false));
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
        assert!(matches!(
            clock.poll(0, &[], false, false),
            Some(Poll::Resync { head: 4, .. })
        ));
        // A reader at the floor has already seen everything evicted.
        let (changes, _, _) = ready(clock.poll(1, &[], false, false));
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
        let (changes, _, _) = ready(clock.poll(0, &[], false, false));
        assert_eq!(changes.len(), 2);
    }

    fn text_poll(clock: &ChangeClock, after: u64, filter: &[String]) -> (Vec<EventLiveText>, u64) {
        match clock.poll(after, filter, true, false) {
            Some(Poll::Ready { text, cursor, .. }) => (text, cursor),
            _ => panic!("expected ready"),
        }
    }
    fn charged(clock: &ChangeClock, thread: &str) -> usize {
        clock.lock().text[thread].charged
    }

    #[test]
    fn text_shares_the_sequence_without_becoming_a_change() {
        let clock = ChangeClock::new();
        clock.record("t");
        clock.record_text("t", "a1", 0, "he", false);
        clock.record_text("t", "a1", 2, "llo", false);
        clock.record_text("t", "a1", 5, "hm", true);
        clock.record_text("t", "a1", 7, "m", true);
        clock.record_text("t", "a1", 8, "m", true);
        clock.record_text("t", "a1", 9, "!", false);
        clock.record_text("t", "a1", 10, "", false);
        // Plain poll: no text, one change, cursor past the text.
        let (changes, _, cursor) = ready(clock.poll(1, &[], false, false));
        assert!(changes.is_empty());
        assert_eq!(cursor, 7);
        // From seq 2 on: answer text merges; reasoning is excluded but offsets stay global.
        let (text, _) = text_poll(&clock, 1, &[]);
        let deltas = &text[0].deltas;
        assert_eq!(text[0].attempt_id, "a1");
        assert!(!text[0].gap);
        assert_eq!(
            deltas
                .iter()
                .map(|d| (d.offset, d.reasoning, d.text.as_str()))
                .collect::<Vec<_>>(),
            [(0, false, "hello!")]
        );
        let (text, cursor) = text_poll(&clock, 6, &[]);
        assert_eq!(
            text[0]
                .deltas
                .iter()
                .map(|d| (d.offset, d.reasoning, d.text.as_str()))
                .collect::<Vec<_>>(),
            [(9, false, "!")]
        );
        assert_eq!(cursor, 7);
    }

    #[test]
    fn reasoning_activity_reports_attempt_start_elapsed_and_observed_bytes_only() {
        let clock = ChangeClock::new();
        let changed = Notify::new();
        let guard = clock.live_text_guard("thread", "attempt", &changed);
        let Some(Poll::Ready {
            cursor: start_cursor,
            reasoning_activity,
            reasoning_changed,
            ..
        }) = clock.poll(0, &[], false, true)
        else {
            panic!("expected start");
        };
        assert!(reasoning_changed);
        assert_eq!(reasoning_activity.len(), 1);
        assert_eq!(reasoning_activity[0].attempt_id, "attempt");
        assert_eq!(reasoning_activity[0].received_bytes, None);
        let wire = serde_json::to_value(&reasoning_activity[0]).unwrap();
        assert!(wire.get("received_bytes").is_none());
        assert!(wire.get("text").is_none());
        assert!(reasoning_activity[0].elapsed_ms < 1_000);

        // Counts received UTF-8 reasoning bytes, not tokens or retained preview bytes.
        clock.record_text("thread", "attempt", 0, "秘密", true);
        let Some(Poll::Ready {
            reasoning_activity,
            reasoning_changed,
            ..
        }) = clock.poll(start_cursor, &[], false, true)
        else {
            panic!("expected reasoning delta");
        };
        assert!(reasoning_changed);
        assert_eq!(
            reasoning_activity[0].received_bytes,
            Some("秘密".len() as u64)
        );
        assert!(
            clock
                .poll(0, &["other".into()], false, true)
                .is_some_and(|poll| match poll {
                    Poll::Ready {
                        reasoning_activity, ..
                    } => reasoning_activity.is_empty(),
                    _ => false,
                })
        );
        drop(guard);
        assert!(
            clock
                .poll(start_cursor, &[], false, true)
                .is_some_and(|poll| match poll {
                    Poll::Ready {
                        reasoning_activity, ..
                    } => reasoning_activity.is_empty(),
                    _ => false,
                })
        );
    }

    #[test]
    fn eviction_holds_the_bound_for_any_delta_size_and_reports_the_gap() {
        for size in [1, 7, 1024, MAX_ATTEMPT_TEXT_BYTES / 2] {
            let clock = ChangeClock::new();
            let piece = "x".repeat(size);
            let mut offset = 0;
            for _ in 0..(4 * MAX_ATTEMPT_TEXT_BYTES / size.max(64)).max(4) {
                clock.record_text("t", "a", offset, &piece, false);
                offset += size as u64;
                assert!(charged(&clock, "t") <= MAX_ATTEMPT_TEXT_BYTES);
            }
            let (text, cursor) = text_poll(&clock, 0, &[]);
            assert!(text[0].gap, "size {size}");
            let joined: usize = text[0].deltas.iter().map(|d| d.text.len()).sum();
            assert_eq!(text[0].deltas[0].offset + joined as u64, offset);
            // Having read the tail there is no gap until more is lost.
            let (text, _) = text_poll(&clock, cursor, &[]);
            assert!(!text[0].gap);
        }
    }

    #[test]
    fn an_oversized_delta_keeps_a_whole_character_tail_and_exact_offset() {
        let clock = ChangeClock::new();
        // Four-byte characters: any cut that is not on a boundary would panic.
        let text = "😀".repeat(MAX_ATTEMPT_TEXT_BYTES);
        clock.record_text("t", "a", 10, &text, false);
        assert!(charged(&clock, "t") <= MAX_ATTEMPT_TEXT_BYTES);
        let (attempts, _) = text_poll(&clock, 0, &[]);
        assert!(attempts[0].gap);
        let delta = &attempts[0].deltas[0];
        assert!(delta.text.chars().all(|c| c == '😀'));
        assert_eq!(
            delta.offset + delta.text.chars().count() as u64,
            10 + MAX_ATTEMPT_TEXT_BYTES as u64
        );
    }

    #[test]
    fn a_new_attempt_replaces_the_old_and_a_stale_guard_frees_nothing_new() {
        let clock = ChangeClock::new();
        let changed = Notify::new();
        let old = clock.live_text_guard("t", "old", &changed);
        assert!(old.record(0, "abc", false));
        let current = clock.live_text_guard("t", "new", &changed);
        assert!(current.record(0, "z", false));
        drop(old);
        let (attempts, _) = text_poll(&clock, 0, &[]);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].attempt_id, "new");
        assert_eq!(attempts[0].deltas[0].offset, 0);
        assert!(!attempts[0].gap);
        drop(current);
        assert!(text_poll(&clock, 0, &[]).0.is_empty());
        assert!(clock.lock().text.is_empty());
    }

    #[test]
    fn past_the_attempt_cap_newcomers_wait_and_incumbents_keep_their_previews() {
        let clock = ChangeClock::new();
        let changed = Notify::new();
        let threads = MAX_LIVE_TEXT_ATTEMPTS + 1;
        let mut at = 0;
        for round in 0..200u64 {
            for i in 0..threads {
                clock.record_text(&format!("t{i:02}"), "a", round, "x", false);
            }
            let (attempts, cursor) = text_poll(&clock, at, &[]);
            at = cursor;
            assert_eq!(attempts.len(), MAX_LIVE_TEXT_ATTEMPTS);
            let late = format!("t{MAX_LIVE_TEXT_ATTEMPTS:02}");
            assert!(!attempts.iter().any(|a| a.thread_id == late));
            // The first read joins at offset 0; every later one is continuous.
            assert!(
                attempts
                    .iter()
                    .all(|a| !a.gap && a.deltas[0].offset == round)
            );
        }
        // An ending attempt makes room: the refused one enters with an explicit gap.
        drop(clock.live_text_guard("t00", "a", &changed));
        let late = format!("t{MAX_LIVE_TEXT_ATTEMPTS:02}");
        clock.record_text(&late, "a", 200, "y", false);
        let (attempts, _) = text_poll(&clock, at, &[]);
        let entered = attempts.iter().find(|a| a.thread_id == late).unwrap();
        assert!(entered.gap);
        assert_eq!(entered.deltas[0].offset, 200);
        assert_eq!(clock.lock().text.len(), MAX_LIVE_TEXT_ATTEMPTS);
    }

    #[test]
    fn filter_limits_text_but_the_cursor_moves_past_it() {
        let clock = ChangeClock::new();
        clock.record_text("a", "1", 0, "aa", false);
        clock.record_text("b", "2", 0, "bb", false);
        let (attempts, cursor) = text_poll(&clock, 0, &["b".into()]);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].thread_id, "b");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn a_paged_change_reply_only_returns_text_up_to_its_cursor() {
        let clock = ChangeClock::new();
        let changed = Notify::new();
        for i in 0..MAX_EVENT_CHANGES {
            clock.record(&format!("t{i}"));
        }
        clock.record_text("attempt-thread", "attempt", 0, "x", false);
        let text_start_seq = clock.lock().text["attempt-thread"].deltas[0].seq;
        let guard = clock.live_text_guard("attempt-thread", "attempt", &changed);
        let start_seq = clock.lock().reasoning_activity["attempt-thread"].sequence;
        clock.record_text("attempt-thread", "attempt", 1, "x", true);
        for i in 0..MAX_EVENT_CHANGES {
            clock.record(&format!("v{i}"));
        }
        for i in 0..(MAX_EVENT_CHANGES - 4) {
            clock.record(&format!("w{i}"));
        }
        clock.record("attempt-thread");
        let Some(Poll::Ready {
            changes,
            more,
            cursor,
            reasoning_activity,
            ..
        }) = clock.poll(0, &[], false, true)
        else {
            panic!("expected first page")
        };
        assert!(more);
        assert_eq!(changes.len(), MAX_EVENT_CHANGES);
        assert!(cursor < start_seq);
        assert_eq!(cursor, MAX_EVENT_CHANGES as u64);
        assert!(text_start_seq > 0);
        assert!(
            reasoning_activity.is_empty(),
            "future attempt leaked into first page"
        );
        let Some(Poll::Ready {
            changes,
            more,
            cursor,
            reasoning_activity,
            ..
        }) = clock.poll(cursor, &[], false, true)
        else {
            panic!("expected second page")
        };
        assert!(more);
        assert_eq!(changes.len(), MAX_EVENT_CHANGES);
        assert!(cursor >= start_seq);
        assert_eq!(reasoning_activity.len(), 1);
        assert_eq!(reasoning_activity[0].attempt_id, "attempt");
        drop(guard);
        let end_seq = clock.lock().seq;
        assert!(cursor < end_seq, "attempt end must be on a later page");
        let Some(Poll::Ready {
            changes,
            more,
            cursor,
            reasoning_activity,
            ..
        }) = clock.poll(cursor, &[], false, true)
        else {
            panic!("expected final page")
        };
        assert!(!more);
        assert!(changes.iter().any(|(_, seq)| *seq == end_seq));
        assert!(
            reasoning_activity.is_empty(),
            "ended attempt appeared before its end cursor"
        );
        assert_eq!(cursor, end_seq);
    }

    #[test]
    fn the_largest_possible_reply_fits_one_frame() {
        let clock = ChangeClock::new();
        // Control characters take six JSON bytes each: the worst retained text.
        let piece = "\u{1}".repeat(MAX_ATTEMPT_TEXT_BYTES);
        let id = "i".repeat(128);
        for i in 0..MAX_LIVE_TEXT_ATTEMPTS {
            clock.record_text(&format!("{id}{i}"), &id, 1, &piece, false);
        }
        for i in 0..MAX_EVENT_CHANGES {
            clock.record(&format!("{id}{i}"));
        }
        let Some(Poll::Ready { changes, text, .. }) = clock.poll(0, &[], true, true) else {
            panic!("expected ready")
        };
        let reply = serde_json::to_vec(&crate::protocol::EventsWaitResult {
            status: crate::protocol::EventsWaitStatus::Changed,
            cursor: "c".repeat(1024),
            changes: changes
                .into_iter()
                .map(|(thread_id, seq)| crate::protocol::EventChange { thread_id, seq })
                .collect(),
            more: false,
            live_text: Some(text),
            reasoning_activity: vec![],
        })
        .unwrap();
        assert!(reply.len() <= MAX_FRAME_BYTES, "{}", reply.len());
        // Largest charged state, delta records included, stays under the stated total.
        let total: usize = clock.lock().text.values().map(|t| t.charged).sum();
        assert!(total <= MAX_LIVE_TEXT_ATTEMPTS * MAX_ATTEMPT_TEXT_BYTES);
    }

    #[tokio::test]
    async fn a_delta_and_its_poll_wake_only_text_waiters_until_a_shutdown_drain() {
        use crate::protocol::{Command, EventsWaitArgs, Request};
        use futures_util::FutureExt;
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let command = |after: Option<String>| {
            Command::EventsWait(EventsWaitArgs {
                after,
                thread_ids: vec![],
                timeout_ms: 0,
                include_text: true,
                include_reasoning_activity: false,
            })
        };
        let changed = control.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let text = control.text_changed.notified();
        tokio::pin!(text);
        text.as_mut().enable();
        control.stream_delta("t", "a", "x", false, 0);
        assert!(text.as_mut().now_or_never().is_some());
        // A whole text poll completes, returning that delta: still no wake for `changed`.
        let reply = control
            .request(Request::new(
                uuid::Uuid::new_v4().to_string(),
                command(None),
            ))
            .await;
        assert_eq!(reply.result.unwrap()["status"], "baseline");
        assert!(changed.as_mut().now_or_never().is_none());
        // Once admission is closed a finishing poll wakes the drain, like any other request.
        let open = control.admit_request(&command(None)).unwrap();
        let closing = control.admit_request(&command(None)).unwrap();
        let quiet = control.changed.notified();
        tokio::pin!(quiet);
        quiet.as_mut().enable();
        drop(open);
        assert!(quiet.as_mut().now_or_never().is_none());
        control.freeze_owner_admission();
        let drain = control.changed.notified();
        tokio::pin!(drain);
        drain.as_mut().enable();
        drop(closing);
        assert!(drain.as_mut().now_or_never().is_some());
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn saved_response_change_wakes_events_wait_with_durable_cursor() {
        use crate::protocol::{Command, EventCursor, EventsWaitArgs, Request};
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let wait = |after| {
            Request::new(
                uuid::Uuid::new_v4().to_string(),
                Command::EventsWait(EventsWaitArgs {
                    after,
                    thread_ids: vec!["thread".into()],
                    timeout_ms: 3_600_000,
                    include_text: false,
                    include_reasoning_activity: false,
                }),
            )
        };
        let baseline = control.request(wait(None)).await.result.unwrap();
        let cursor = baseline["cursor"].as_str().unwrap().to_owned();
        let after = EventCursor::decode(&cursor).unwrap();
        let waiting = {
            let control = control.clone();
            tokio::spawn(async move { control.request(wait(Some(cursor))).await })
        };
        tokio::task::yield_now().await;
        // This is the same post-commit hook used by response adoption: it records
        // before notify, so an events.wait waiter can read the new sequence.
        control.changed("thread");
        let response = waiting.await.unwrap();
        assert!(response.ok, "{:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["status"], "changed");
        assert_eq!(result["changes"][0]["thread_id"], "thread");
        let next = EventCursor::decode(result["cursor"].as_str().unwrap()).unwrap();
        assert!(next.seq > after.seq);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn events_wait_reports_reasoning_activity_start_bytes_and_end() {
        use crate::protocol::{Command, EventsWaitArgs, Request};
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let wait = |after| {
            Request::new(
                uuid::Uuid::new_v4().to_string(),
                Command::EventsWait(EventsWaitArgs {
                    after,
                    thread_ids: vec!["t".into()],
                    timeout_ms: 3_600_000,
                    include_text: false,
                    include_reasoning_activity: true,
                }),
            )
        };
        let baseline_response = control.request(wait(None)).await;
        assert!(baseline_response.ok, "{:?}", baseline_response.error);
        let baseline = baseline_response.result.unwrap();
        let cursor = baseline["cursor"].as_str().unwrap().to_owned();
        let guard = control
            .change_clock
            .live_text_guard("t", "attempt", &control.text_changed);
        let started_response = control.request(wait(Some(cursor.clone()))).await;
        assert!(started_response.ok, "{:?}", started_response.error);
        let started = started_response.result.unwrap();
        assert_eq!(started["status"], "text");
        assert_eq!(started["reasoning_activity"][0]["attempt_id"], "attempt");
        assert_eq!(
            started["reasoning_activity"][0]["received_bytes"],
            Value::Null
        );
        let cursor = started["cursor"].as_str().unwrap().to_owned();

        control.stream_delta("t", "attempt", "秘密", true, 0);
        let progress_response = control.request(wait(Some(cursor))).await;
        assert!(progress_response.ok, "{:?}", progress_response.error);
        let progress = progress_response.result.unwrap();
        assert_eq!(progress["status"], "text");
        assert_eq!(
            progress["reasoning_activity"][0]["received_bytes"],
            "秘密".len() as u64
        );
        assert_eq!(progress.get("live_text"), None);
        assert_eq!(progress["reasoning_activity"][0].get("text"), None);
        let cursor = progress["cursor"].as_str().unwrap().to_owned();
        drop(guard);
        let ended_response = control.request(wait(Some(cursor))).await;
        assert!(ended_response.ok, "{:?}", ended_response.error);
        let ended = ended_response.result.unwrap();
        assert_eq!(ended["status"], "changed");
        assert_eq!(ended["changes"][0]["thread_id"], "t");
        assert_eq!(ended["reasoning_activity"], serde_json::json!([]));
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    /// Lock-step: each delta is written just as the reader re-enters its wait, the window a
    /// lost wake-up would need. The timeout is far beyond the budget, so only a wake-up can
    /// end each poll; a lost one fails the watchdog rather than being rescued by a timeout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn every_delta_wakes_a_waiting_text_poll_under_racing_writes() {
        use crate::protocol::{Command, EventsWaitArgs, Request};
        const DELTAS: u64 = 3000;
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let wait = |after: Option<String>, thread: &str, include_text| {
            Request::new(
                uuid::Uuid::new_v4().to_string(),
                Command::EventsWait(EventsWaitArgs {
                    after,
                    thread_ids: vec![thread.to_owned()],
                    timeout_ms: 3_600_000,
                    include_text,
                    include_reasoning_activity: false,
                }),
            )
        };
        let result = control.request(wait(None, "t", true)).await.result.unwrap();
        let mut at = result["cursor"].as_str().unwrap().to_owned();
        // A plain poll elsewhere must stay asleep throughout and still get its hint at the end.
        let plain_at = at.clone();
        let plain = {
            let control = control.clone();
            let request = wait(Some(plain_at), "other", false);
            tokio::spawn(async move { control.request(request).await })
        };
        let (progress, mut seen) = tokio::sync::watch::channel(0u64);
        let writer = {
            let control = control.clone();
            tokio::spawn(async move {
                for offset in 0..DELTAS {
                    while *seen.borrow_and_update() < offset {
                        seen.changed().await.unwrap();
                    }
                    control.stream_delta("t", "a", "x", false, offset as usize);
                }
            })
        };
        let finished = crate::test_watchdog::watchdog("every delta to wake the poll", async {
            let mut total = 0u64;
            while total < DELTAS {
                let response = control.request(wait(Some(at.clone()), "t", true)).await;
                assert!(response.ok, "{:?}", response.error);
                let value = response.result.unwrap();
                at = value["cursor"].as_str().unwrap().to_owned();
                assert_eq!(value["status"], "text");
                let attempt = &value["live_text"][0];
                assert_eq!(attempt["gap"], false);
                for delta in attempt["deltas"].as_array().unwrap() {
                    assert_eq!(delta["offset"].as_u64().unwrap(), total);
                    total += delta["text"].as_str().unwrap().len() as u64;
                }
                // The writer has gone once it wrote the last delta.
                let _ = progress.send(total);
            }
        })
        .await;
        assert!(finished.is_ok(), "a text poll missed a wake-up");
        writer.await.unwrap();
        assert!(!plain.is_finished(), "text ended a plain poll");
        control.changed("other");
        let response = crate::test_watchdog::watchdog("plain poll hint", plain)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.result.unwrap()["status"], "changed");
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    /// Polls finishing while shutdown drains: a skipped wake would park the drain for good.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_drains_while_text_polls_are_finishing() {
        use crate::protocol::{Command, EventsWaitArgs, Request};
        for _ in 0..8 {
            let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
            let mut pollers = Vec::new();
            for _ in 0..16 {
                let control = control.clone();
                pollers.push(tokio::spawn(async move {
                    loop {
                        let reply = control
                            .request(Request::new(
                                uuid::Uuid::new_v4().to_string(),
                                Command::EventsWait(EventsWaitArgs {
                                    after: None,
                                    thread_ids: vec![],
                                    timeout_ms: 0,
                                    include_text: true,
                                    include_reasoning_activity: true,
                                }),
                            ))
                            .await;
                        if !reply.ok {
                            return;
                        }
                    }
                }));
            }
            tokio::task::yield_now().await;
            server.shutdown().await.unwrap();
            let drained = crate::test_watchdog::watchdog("drain with polls", control.shutdown());
            drained.await.unwrap().unwrap();
            for poller in pollers {
                poller.await.unwrap();
            }
            owner.shutdown().await.unwrap();
        }
    }

    #[test]
    fn cursor_ahead_of_the_clock_is_rejected() {
        let clock = ChangeClock::new();
        clock.record("a");
        assert!(clock.poll(2, &[], false, false).is_none());
    }
}

#[cfg(test)]
mod admission_fence_tests {
    use super::*;
    #[test]
    fn admission_without_text_keeps_text_baseline_empty_but_reasoning_active() {
        let clock = ChangeClock::new();
        let notify = Notify::new();
        let guard = clock.live_text_guard("t", "a", &notify);
        let before = clock.snapshot(&[], true, true);
        assert!(before.1.is_empty());
        assert_eq!(before.2.len(), 1);
        assert!(guard.record(0, "first", false));
        assert_eq!(clock.snapshot(&[], true, true).1.len(), 1);
    }
    #[test]
    fn admitted_newcomer_at_capacity_has_no_live_publication_authority() {
        let clock = ChangeClock::new();
        let notify = Notify::new();
        let ids = (0..17).map(|i| format!("t{i}")).collect::<Vec<_>>();
        let guards = ids
            .iter()
            .map(|id| clock.live_text_guard(id, id, &notify))
            .collect::<Vec<_>>();
        for guard in guards.iter().take(16) {
            assert!(guard.record(0, "received", false));
        }
        let before = clock.snapshot(&[], true, true);
        assert!(!guards[16].record(0, "denied text", false));
        assert!(!guards[16].record(0, "denied reasoning", true));
        assert_eq!(clock.snapshot(&[], true, true).1.len(), 16);
        assert_eq!(clock.snapshot(&[], true, true).2.len(), before.2.len());
        assert!(guards[0].record(0, "current", false));
    }
    #[test]
    fn terminal_revoke_is_update_only_for_text_and_reasoning() {
        let clock = ChangeClock::new();
        let notify = Notify::new();
        let guard = clock.live_text_guard("t", "a", &notify);
        assert!(guard.record(0, "text", false));
        assert!(guard.record(4, "thinking", true));
        let old_poll = clock.snapshot(&[], true, true);
        clock.revoke_live("t", "a", None);
        assert!(!guard.record(0, "late", false));
        assert!(!guard.record(0, "late", true));
        let fresh = clock.snapshot(&[], true, true);
        assert!(fresh.1.is_empty() && fresh.2.is_empty());
        assert_eq!(old_poll.1.len(), 1); // Already materialized responses remain legal.
    }
    #[test]
    fn duplicate_guard_old_drop_and_old_write_cannot_touch_new_token() {
        let clock = ChangeClock::new();
        let notify = Notify::new();
        let old = clock.live_text_guard("t", "a", &notify);
        let current = clock.live_text_guard("t", "a", &notify);
        assert!(!old.record(0, "old", false));
        drop(old);
        assert!(current.record(0, "current", false));
        assert_eq!(
            clock.snapshot(&[], true, true).1[0].deltas[0].text,
            "current"
        );
        let next = clock.live_text_guard("t", "b", &notify);
        drop(current);
        assert!(next.record(0, "next", false));
    }
    #[test]
    fn abandoned_admission_and_evicted_guard_cannot_recreate_preview() {
        let clock = ChangeClock::new();
        let notify = Notify::new();
        let guard = clock.reserve_live("t", "a", &notify);
        let (token, alive) = guard.reservation();
        drop(guard);
        clock.admit_live("t", "a", &token, &alive);
        assert!(clock.snapshot(&[], true, true).1.is_empty());
        let guard = clock.live_text_guard("t", "a", &notify);
        {
            clock.lock().text.remove("t");
        }
        assert!(!guard.record(0, "after eviction", false));
    }
    #[test]
    fn bounded_admission_and_other_thread_survive_exact_revoke() {
        let clock = ChangeClock::new();
        let notify = Notify::new();
        let ids = (0..17).map(|i| format!("t{i}")).collect::<Vec<_>>();
        let guards = ids
            .iter()
            .map(|id| clock.live_text_guard(id, id, &notify))
            .collect::<Vec<_>>();
        assert_eq!(clock.snapshot(&[], true, true).2.len(), 16);
        assert!(!guards[16].record(0, "over cap", true));
        clock.revoke_live("t0", "t0", None);
        assert!(guards[1].record(0, "unaffected", true));
        assert!(!guards[16].record(0, "not readmitted", false));
    }
}
