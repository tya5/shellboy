//! In-memory per-Thread change clock behind `events.wait` (#243).
//!
//! Owner: `Control` (dropped with it). No task, queue, table or DB access.
//! Bound: at most `MAX_CHANGE_THREADS` entries (latest sequence per Thread);
//! older entries are evicted and raise `floor`, so a reader positioned below it
//! is told to resynchronise instead of silently missing changes.
//!
//! The same lock and sequence also carry unsaved model text for clients that opt
//! in (#243 E3): one lossy tail per streaming attempt, freed when the attempt ends
//! (`LiveTextGuard`), never saved and never a source of truth. Writers never wait
//! for a reader: the lock is held only for memory updates and copies, never across
//! `.await` or I/O, and a reader that stops reading holds nothing here.
use crate::protocol::{EventLiveText, EventTextDelta, MAX_EVENT_CHANGES, MAX_FRAME_BYTES};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Mutex,
};

pub(crate) const MAX_CHANGE_THREADS: usize = 4096;
/// Streaming attempts holding text at once; the least recently written is dropped.
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
}

pub(crate) struct ChangeClock {
    inner: Mutex<Inner>,
    cap: usize,
}

pub(crate) enum Poll {
    /// Cursor older than the retained window.
    Resync { head: u64, text: Vec<EventLiveText> },
    Ready {
        changes: Vec<(String, u64)>,
        more: bool,
        /// Position to continue from: last returned change when `more`, else the head.
        cursor: u64,
        /// Live attempts and their text up to `cursor`; empty unless asked for.
        text: Vec<EventLiveText>,
    },
}

/// Frees an attempt's retained text however its stream ends (return, cancel, drop).
pub(crate) struct LiveTextGuard<'a> {
    clock: &'a ChangeClock,
    thread: &'a str,
    attempt: &'a str,
}
impl Drop for LiveTextGuard<'_> {
    fn drop(&mut self) {
        let mut inner = self.clock.lock();
        if inner
            .text
            .get(self.thread)
            .is_some_and(|text| text.attempt == self.attempt)
        {
            inner.text.remove(self.thread);
        }
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
    pub(crate) fn live_text_guard<'a>(
        &'a self,
        thread: &'a str,
        attempt: &'a str,
    ) -> LiveTextGuard<'a> {
        LiveTextGuard {
            clock: self,
            thread,
            attempt,
        }
    }
    /// `offset` counts the Unicode scalar values already streamed by this attempt.
    pub(crate) fn record_text(
        &self,
        thread: &str,
        attempt: &str,
        offset: u64,
        text: &str,
        reasoning: bool,
    ) {
        if text.is_empty() {
            return;
        }
        let mut inner = self.lock();
        inner.seq += 1;
        let seq = inner.seq;
        if inner
            .text
            .get(thread)
            .is_none_or(|current| current.attempt != attempt)
        {
            if !inner.text.contains_key(thread) && inner.text.len() >= MAX_LIVE_TEXT_ATTEMPTS {
                let oldest = inner
                    .text
                    .iter()
                    .min_by_key(|(_, text)| text.deltas.back().map_or(0, |delta| delta.seq))
                    .map(|(thread, _)| thread.clone());
                if let Some(oldest) = oldest {
                    inner.text.remove(&oldest);
                }
            }
            inner.text.insert(
                thread.to_owned(),
                AttemptText {
                    attempt: attempt.to_owned(),
                    deltas: VecDeque::new(),
                    charged: 0,
                    // Entered mid-attempt after being dropped: earlier text is gone.
                    floor: if offset == 0 { 0 } else { seq },
                },
            );
        }
        let entry = inner.text.get_mut(thread).expect("entry inserted above");
        // A single oversized delta keeps its tail, cut on a character boundary.
        let room = MAX_ATTEMPT_TEXT_BYTES - DELTA_RECORD_BYTES;
        let start = text.ceil_char_boundary(text.len().saturating_sub(room));
        let offset = offset + text[..start].chars().count() as u64;
        if start > 0 {
            entry.floor = seq;
        }
        let kept = &text[start..];
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
    }
    /// The head and, when asked for, everything retained: for a client with no position.
    pub(crate) fn snapshot(
        &self,
        filter: &[String],
        include_text: bool,
    ) -> (u64, Vec<EventLiveText>) {
        let inner = self.lock();
        let text = if include_text {
            inner.live_text(0, inner.seq, filter)
        } else {
            Vec::new()
        };
        (inner.seq, text)
    }
    /// `None` when `after` is ahead of the clock (a forged or corrupt cursor).
    pub(crate) fn poll(&self, after: u64, filter: &[String], include_text: bool) -> Option<Poll> {
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
            return Some(Poll::Resync {
                head: inner.seq,
                text,
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
        Some(Poll::Ready {
            changes,
            more,
            cursor,
            text,
        })
    }
}

impl Inner {
    /// Every live attempt in the filter with its text in `(after, upto]`. Adjacent
    /// deltas of one kind merge; a listed attempt with no text is still live.
    fn live_text(&self, after: u64, upto: u64, filter: &[String]) -> Vec<EventLiveText> {
        let mut attempts = Vec::new();
        for (thread, entry) in &self.text {
            if !filter.is_empty() && !filter.iter().any(|wanted| wanted == thread) {
                continue;
            }
            let mut deltas: Vec<EventTextDelta> = Vec::new();
            let start = entry.deltas.partition_point(|delta| delta.seq <= after);
            for delta in entry.deltas.range(start..) {
                if delta.seq > upto {
                    break;
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
        let (changes, more, cursor) = ready(clock.poll(0, &[], false));
        assert_eq!(
            changes,
            vec![("b".into(), 2), ("a".into(), 4), ("c".into(), 5)]
        );
        assert!(!more);
        assert_eq!(cursor, 5);
        let (changes, _, _) = ready(clock.poll(2, &[], false));
        assert_eq!(changes.len(), 2);
        let (none, _, cursor) = ready(clock.poll(5, &[], false));
        assert!(none.is_empty());
        assert_eq!(cursor, 5);
    }

    #[test]
    fn filter_limits_changes_but_cursor_still_reaches_head() {
        let clock = ChangeClock::new();
        clock.record("a");
        clock.record("b");
        let (changes, _, cursor) = ready(clock.poll(0, &["b".into()], false));
        assert_eq!(changes, vec![("b".into(), 2)]);
        assert_eq!(cursor, 2);
        let (changes, _, cursor) = ready(clock.poll(0, &["zzz".into()], false));
        assert!(changes.is_empty());
        assert_eq!(cursor, 2);
    }

    #[test]
    fn page_of_sixty_four_reports_more_and_resumes_without_a_gap() {
        let clock = ChangeClock::new();
        for i in 0..100 {
            clock.record(&format!("t{i}"));
        }
        let (first, more, cursor) = ready(clock.poll(0, &[], false));
        assert_eq!(first.len(), MAX_EVENT_CHANGES);
        assert!(more);
        assert_eq!(cursor, 64);
        let (rest, more, cursor) = ready(clock.poll(cursor, &[], false));
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
            clock.poll(0, &[], false),
            Some(Poll::Resync { head: 4, .. })
        ));
        // A reader at the floor has already seen everything evicted.
        let (changes, _, _) = ready(clock.poll(1, &[], false));
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
        let (changes, _, _) = ready(clock.poll(0, &[], false));
        assert_eq!(changes.len(), 2);
    }

    fn text_poll(clock: &ChangeClock, after: u64, filter: &[String]) -> (Vec<EventLiveText>, u64) {
        match clock.poll(after, filter, true) {
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
        clock.record_text("t", "a1", 7, "!", false);
        clock.record_text("t", "a1", 8, "", false);
        // Plain poll: no text, one change, cursor past the text.
        let (changes, _, cursor) = ready(clock.poll(1, &[], false));
        assert!(changes.is_empty());
        assert_eq!(cursor, 5);
        // From seq 2 on: a run of one kind merges; kinds keep their order and offsets.
        let (text, _) = text_poll(&clock, 2, &[]);
        let deltas = &text[0].deltas;
        assert_eq!(text[0].attempt_id, "a1");
        assert!(!text[0].gap);
        assert_eq!(
            deltas
                .iter()
                .map(|d| (d.offset, d.reasoning, d.text.as_str()))
                .collect::<Vec<_>>(),
            [(2, false, "llo"), (5, true, "hm"), (7, false, "!")]
        );
        let (text, cursor) = text_poll(&clock, 5, &[]);
        assert!(text[0].deltas.is_empty());
        assert_eq!(cursor, 5);
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
        {
            let _old = clock.live_text_guard("t", "old");
            clock.record_text("t", "old", 0, "abc", false);
            clock.record_text("t", "new", 0, "z", false);
            let (attempts, _) = text_poll(&clock, 0, &[]);
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].attempt_id, "new");
            assert_eq!(attempts[0].deltas[0].offset, 0);
            assert!(!attempts[0].gap);
        }
        assert_eq!(text_poll(&clock, 0, &[]).0[0].attempt_id, "new");
        drop(clock.live_text_guard("t", "new"));
        assert!(text_poll(&clock, 0, &[]).0.is_empty());
        assert!(clock.lock().text.is_empty());
    }

    #[test]
    fn past_the_attempt_cap_the_least_recent_is_dropped_and_reenters_with_a_gap() {
        let clock = ChangeClock::new();
        for i in 0..MAX_LIVE_TEXT_ATTEMPTS {
            clock.record_text(&format!("t{i}"), "a", 0, "x", false);
        }
        clock.record_text("t0", "a", 1, "y", false);
        clock.record_text("extra", "a", 0, "x", false);
        let (attempts, cursor) = text_poll(&clock, 0, &[]);
        assert_eq!(attempts.len(), MAX_LIVE_TEXT_ATTEMPTS);
        // t1 was written least recently; its absence says no text is held for it.
        assert!(!attempts.iter().any(|a| a.thread_id == "t1"));
        assert!(attempts.iter().any(|a| a.thread_id == "t0"));
        clock.record_text("t1", "a", 1, "more", false);
        let (attempts, _) = text_poll(&clock, cursor, &[]);
        let back = attempts.iter().find(|a| a.thread_id == "t1").unwrap();
        assert!(back.gap);
        assert_eq!(back.deltas[0].offset, 1);
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
        for i in 0..MAX_EVENT_CHANGES {
            clock.record(&format!("t{i}"));
            clock.record_text("s", "a", i as u64, "x", false);
        }
        clock.record("last");
        clock.record_text("s", "a", 64, "x", false);
        let Some(Poll::Ready {
            changes,
            more,
            cursor,
            text,
        }) = clock.poll(0, &[], true)
        else {
            panic!("expected ready")
        };
        assert_eq!(changes.len(), MAX_EVENT_CHANGES);
        assert!(more);
        assert_eq!(cursor, changes.last().unwrap().1);
        let first: String = text[0].deltas.iter().map(|d| d.text.clone()).collect();
        let (rest, _) = text_poll(&clock, cursor, &[]);
        let second: String = rest[0].deltas.iter().map(|d| d.text.clone()).collect();
        assert_eq!(rest[0].deltas[0].offset as usize, first.len());
        assert_eq!(first.len() + second.len(), 65);
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
        let Some(Poll::Ready { changes, text, .. }) = clock.poll(0, &[], true) else {
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
            .request(Request::new(uuid::Uuid::new_v4().to_string(), command(None)))
            .await;
        assert_eq!(reply.result.unwrap()["status"], "baseline");
        assert!(changed.as_mut().now_or_never().is_none());
        // Once admission is closed a finishing poll wakes the drain, like any other request.
        let open = control.admit_request(&command(None)).unwrap();
        let closing = control.admit_request(&command(None)).unwrap();
        drop(open);
        let quiet = control.changed.notified();
        tokio::pin!(quiet);
        quiet.as_mut().enable();
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
        assert!(clock.poll(2, &[], false).is_none());
    }
}
