//! In-memory per-Thread change clock behind `events.wait` (#243).
//!
//! Owner: `Control` (dropped with it). No task, queue, table or DB access.
//! Bound: at most `MAX_CHANGE_THREADS` entries (latest sequence per Thread);
//! older entries are evicted and raise `floor`, so a reader positioned below it
//! is told to resynchronise instead of silently missing changes.
//!
//! The same lock and sequence also carry unsaved model text and attempt-scoped
//! reasoning activity for clients that opt in (#243 E3, #275), and each shell
//! execution's latest committed-output position (#311): IDs and sequence only,
//! never output bytes, coalesced per execution under the same Thread bound. Both are freed when
//! the attempt ends (`LiveTextGuard`), never saved and never a source of truth.
//! Writers never wait
//! for a reader: the lock is held only for memory updates and copies, never across
//! `.await` or I/O, and a reader that stops reading holds nothing here.
use super::wait::wakes::{Key, Wakes};
use crate::protocol::{
    EventAttemptEnd, EventAttemptOutcome, EventLiveText, EventOutput, EventReasoningActivity,
    EventRecoveryTransition, EventTextDelta, MAX_EVENT_CHANGES, MAX_FRAME_BYTES,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::Mutex,
};

pub(crate) const MAX_CHANGE_THREADS: usize = 4096;
// A hard count bound. Eviction advances the live cursor floor so clients resync
// from saved history instead of guessing which attempt was discarded.
const MAX_ATTEMPT_ENDS: usize = 256;
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
    input_through_node_id: Option<String>,
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
    /// Latest owner-wide marks; every client compares them to its own cursor.
    config_changed_seq: Option<u64>,
    index_changed_seq: Option<u64>,
    retention_changed_seq: Option<u64>,
    by_thread: HashMap<String, u64>,
    by_seq: BTreeMap<u64, String>,
    /// Latest committed-output sequence per execution, with its Thread.
    output_by_execution: HashMap<String, u64>,
    output_by_seq: BTreeMap<u64, (String, String)>,
    recovery: VecDeque<EventRecoveryTransition>,
    /// Keyed by Thread: a Thread streams one attempt at a time, and a later attempt replaces it.
    text: HashMap<String, AttemptText>,
    reasoning_activity: HashMap<String, AttemptActivity>,
    attempt_ends: VecDeque<EventAttemptEnd>,
    attempt_end_floor: u64,
}

struct AttemptActivity {
    token: String,
    attempt: String,
    started: std::time::Instant,
    received_bytes: Option<u64>,
    last_receive: Option<std::time::Instant>,
    sequence: u64,
}

pub(crate) struct ChangeClock {
    wakes: Wakes,
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
        recovery_transitions: Vec<EventRecoveryTransition>,
        config_changed: bool,
        index_changed: bool,
        retention_changed: bool,
        /// Empty unless asked for.
        output: Vec<EventOutput>,
        attempt_ends: Vec<EventAttemptEnd>,
    },
}

/// Owner-wide latest-change marks. Each has no Thread and ignores the Thread filter.
#[derive(Clone, Copy)]
pub(crate) enum Mark {
    Config,
    /// Saved index state or the profile pause changed (`index.status`).
    Index,
    /// GC made a saved source, raw output or managed file unavailable.
    Retention,
}

/// Frees an attempt's retained text however its stream ends (return, cancel, drop).
pub(crate) struct LiveTextGuard<'a> {
    clock: &'a ChangeClock,
    thread: &'a str,
    attempt: &'a str,
    token: String,
    alive: std::sync::Arc<Mutex<bool>>,
}
impl LiveTextGuard<'_> {
    pub(crate) fn reservation(&self) -> (String, std::sync::Arc<Mutex<bool>>) {
        (self.token.clone(), self.alive.clone())
    }
    pub(crate) fn received(&self, at: std::time::Instant) {
        let mut inner = self.clock.lock();
        if let Some(active) = inner
            .reasoning_activity
            .get_mut(self.thread)
            .filter(|active| active.token == self.token)
        {
            // Time since the last bytes is sampled on readback, not cursor progress.
            active.last_receive = Some(active.last_receive.map_or(at, |last| last.max(at)));
        }
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
    }
}

impl ChangeClock {
    pub(super) fn new(wakes: Wakes) -> Self {
        Self::with_registry(MAX_CHANGE_THREADS, wakes)
    }
    #[cfg(test)]
    fn with_capacity(cap: usize) -> Self {
        Self::with_registry(cap, Wakes::default())
    }
    fn with_registry(cap: usize, wakes: Wakes) -> Self {
        Self {
            wakes,
            inner: Mutex::new(Inner::default()),
            cap: cap.max(1),
        }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    pub(crate) fn record_config_changed(&self) {
        self.record_mark(Mark::Config);
    }
    pub(crate) fn record_mark(&self, mark: Mark) {
        let mut inner = self.lock();
        inner.seq += 1;
        let seq = Some(inner.seq);
        match mark {
            Mark::Config => inner.config_changed_seq = seq,
            Mark::Index => inner.index_changed_seq = seq,
            Mark::Retention => inner.retention_changed_seq = seq,
        }
        drop(inner);
        self.wakes.publish([Key::Marks]);
    }
    /// A committed output batch. Eviction raises the shared floor, like Thread entries.
    pub(crate) fn record_output(&self, thread_id: &str, execution_id: &str) {
        let mut inner = self.lock();
        inner.seq += 1;
        let seq = inner.seq;
        if let Some(old) = inner
            .output_by_execution
            .insert(execution_id.to_owned(), seq)
        {
            inner.output_by_seq.remove(&old);
        }
        inner
            .output_by_seq
            .insert(seq, (thread_id.to_owned(), execution_id.to_owned()));
        let old_floor = inner.floor;
        while inner.output_by_execution.len() > self.cap {
            let Some((evicted, (_, execution))) = inner.output_by_seq.pop_first() else {
                break;
            };
            inner.output_by_execution.remove(&execution);
            inner.floor = inner.floor.max(evicted);
        }
        let floor = if inner.floor != old_floor {
            inner.floor
        } else {
            0
        };
        drop(inner);
        self.wakes.publish_clock(
            [Key::Output(Some(thread_id.into())), Key::Output(None)],
            floor,
            0,
        );
    }
    pub(crate) fn record(&self, thread_id: &str) {
        self.record_change(thread_id, false);
    }
    pub(super) fn record_change(&self, thread_id: &str, recovery: bool) {
        let mut inner = self.lock();
        inner.seq += 1;
        let seq = inner.seq;
        if let Some(old) = inner.by_thread.insert(thread_id.to_owned(), seq) {
            inner.by_seq.remove(&old);
        }
        inner.by_seq.insert(seq, thread_id.to_owned());
        let old_floor = inner.floor;
        if recovery {
            inner.recovery.push_back(EventRecoveryTransition {
                thread_id: thread_id.into(),
                seq,
            });
            if inner.recovery.len() > self.cap
                && let Some(evicted) = inner.recovery.pop_front()
            {
                inner.floor = inner.floor.max(evicted.seq);
            }
        }
        while inner.by_thread.len() > self.cap {
            let Some((evicted, thread)) = inner.by_seq.pop_first() else {
                break;
            };
            inner.by_thread.remove(&thread);
            inner.floor = inner.floor.max(evicted);
        }
        let floor = if inner.floor != old_floor {
            inner.floor
        } else {
            0
        };
        drop(inner);
        self.wakes.publish_clock(
            [Key::Changed(Some(thread_id.into())), Key::Changed(None)],
            floor,
            0,
        );
    }
    pub(crate) fn reserve_live<'a>(
        &'a self,
        thread: &'a str,
        attempt: &'a str,
    ) -> LiveTextGuard<'a> {
        LiveTextGuard {
            clock: self,
            thread,
            attempt,
            token: uuid::Uuid::new_v4().to_string(),
            alive: std::sync::Arc::new(Mutex::new(true)),
        }
    }
    // Worker -> reservation -> bounded memory; drop uses the same final two locks.
    pub(crate) fn admit_live(
        &self,
        thread: &str,
        attempt: &str,
        token: &str,
        alive: &Mutex<bool>,
        input_through_node_id: Option<String>,
    ) {
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
                last_receive: None,
                sequence,
            },
        );
        inner.text.insert(
            thread.into(),
            AttemptText {
                attempt: attempt.into(),
                input_through_node_id,
                deltas: VecDeque::new(),
                charged: 0,
                floor: 0,
            },
        );
        drop(inner);
        self.wakes
            .publish([Key::Reasoning(Some(thread.into())), Key::Reasoning(None)]);
    }
    pub(crate) fn record_attempt_end(
        &self,
        thread: &str,
        attempt: &str,
        outcome: EventAttemptOutcome,
        output_limit: Option<crate::protocol::AttemptOutputLimit>,
    ) {
        let mut inner = self.lock();
        inner.seq += 1;
        let seq = inner.seq;
        // An end can race a newer stream, but it can revoke only its own slot.
        if inner
            .reasoning_activity
            .get(thread)
            .is_some_and(|live| live.attempt == attempt)
        {
            inner.reasoning_activity.remove(thread);
            inner.text.remove(thread);
        }
        inner.attempt_ends.push_back(EventAttemptEnd {
            thread_id: thread.into(),
            attempt_id: attempt.into(),
            outcome,
            output_limit,
            seq,
        });
        let old_floor = inner.attempt_end_floor;
        if inner.attempt_ends.len() > MAX_ATTEMPT_ENDS {
            inner.attempt_end_floor = inner
                .attempt_ends
                .pop_front()
                .expect("end queue exceeds bound")
                .seq;
        }
        let floor = if inner.attempt_end_floor != old_floor {
            inner.attempt_end_floor
        } else {
            0
        };
        drop(inner);
        self.wakes.publish_clock(
            [
                Key::AttemptEnd(Some(thread.into())),
                Key::AttemptEnd(None),
                Key::Reasoning(Some(thread.into())),
                Key::Reasoning(None),
            ],
            0,
            floor,
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
    ) -> LiveTextGuard<'a> {
        let guard = self.reserve_live(thread, attempt);
        self.admit_live(thread, attempt, &guard.token, &guard.alive, None);
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
                    input_through_node_id: None,
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
        drop(inner);
        let mut keys = vec![Key::Live(Some(thread.into())), Key::Live(None)];
        if reasoning {
            keys.extend([Key::Reasoning(Some(thread.into())), Key::Reasoning(None)]);
        }
        self.wakes.publish(keys);
        true
    }
    pub(crate) fn thread_position(&self, thread: &str) -> u64 {
        let inner = self.lock();
        inner.by_thread.get(thread).copied().unwrap_or(inner.floor)
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
        include_output: bool,
    ) -> Option<Poll> {
        let inner = self.lock();
        if after > inner.seq {
            return None;
        }
        if after < inner.floor || (include_text && after < inner.attempt_end_floor) {
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
        let mut cursor = if more {
            changes.last().map_or(inner.seq, |(_, seq)| *seq)
        } else {
            inner.seq
        };
        let mut recovery_transitions = Vec::new();
        for recovery in inner
            .recovery
            .iter()
            .filter(|event| {
                event.seq > after
                    && event.seq <= cursor
                    && (filter.is_empty() || filter.contains(&event.thread_id))
            })
            .take(MAX_EVENT_CHANGES + 1)
        {
            recovery_transitions.push(recovery.clone());
        }
        if recovery_transitions.len() > MAX_EVENT_CHANGES {
            recovery_transitions.pop();
            cursor = recovery_transitions
                .last()
                .expect("nonempty recovery page")
                .seq;
            changes.retain(|(_, seq)| *seq <= cursor);
            more = true;
        }
        let mut output = Vec::new();
        if include_output {
            for (seq, (thread, execution)) in inner.output_by_seq.range(after + 1..=cursor) {
                if !filter.is_empty() && !filter.contains(thread) {
                    continue;
                }
                if output.len() == MAX_EVENT_CHANGES {
                    cursor = output.last().map_or(cursor, |last: &EventOutput| last.seq);
                    changes.retain(|(_, seq)| *seq <= cursor);
                    recovery_transitions.retain(|event| event.seq <= cursor);
                    more = true;
                    break;
                }
                output.push(EventOutput {
                    thread_id: thread.clone(),
                    execution_id: execution.clone(),
                    seq: *seq,
                });
            }
        }
        let mark = |seq: Option<u64>| seq.is_some_and(|seq| seq > after && seq <= cursor);
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
        let attempt_ends = if include_text {
            inner
                .attempt_ends
                .iter()
                .filter(|end| {
                    end.seq > after
                        && end.seq <= cursor
                        && (filter.is_empty() || filter.contains(&end.thread_id))
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        Some(Poll::Ready {
            changes,
            more,
            cursor,
            text,
            reasoning_activity,
            reasoning_changed,
            recovery_transitions,
            config_changed: mark(inner.config_changed_seq),
            index_changed: mark(inner.index_changed_seq),
            retention_changed: mark(inner.retention_changed_seq),
            output,
            attempt_ends,
        })
    }
}

#[cfg(test)]
impl ChangeClock {
    pub(crate) fn standalone() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::new(Wakes::default()))
    }
    /// Whether `mark` is newer than `seen`, read as an events.wait caller would;
    /// `seen` moves to the head.
    pub(crate) fn take_mark(&self, mark: Mark, seen: &mut u64) -> bool {
        let mut found = false;
        loop {
            let Some(Poll::Ready {
                config_changed,
                index_changed,
                retention_changed,
                more,
                cursor,
                ..
            }) = self.poll(*seen, &[], false, false, false)
            else {
                panic!("a test cursor stays inside the retained clock");
            };
            found |= match mark {
                Mark::Config => config_changed,
                Mark::Index => index_changed,
                Mark::Retention => retention_changed,
            };
            *seen = cursor;
            if !more {
                return found;
            }
        }
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
        let now = std::time::Instant::now();
        for (thread, entry) in &self.text {
            let receive_age_ms = self
                .reasoning_activity
                .get(thread)
                .and_then(|active| active.last_receive)
                .map(|at| {
                    now.saturating_duration_since(at)
                        .as_millis()
                        .min(u64::MAX as u128) as u64
                });
            // Admission reserves capacity, but is not itself a received text delta.
            // Retired deltas retain a floor, so their empty/gap entries stay visible.
            if entry.deltas.is_empty() && entry.floor == 0 && receive_age_ms.is_none() {
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
                input_through_node_id: entry.input_through_node_id.clone(),
                receive_age_ms,
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
    fn attempt_ends_page_in_clock_order_and_overflow_requires_resync() {
        let clock = ChangeClock::new(Wakes::default());
        for n in 0..MAX_EVENT_CHANGES {
            clock.record(&format!("hint-{n}"));
        }
        clock.record_attempt_end("t", "first", EventAttemptOutcome::Adopted, None);
        clock.record("next-page");
        clock.record_attempt_end("t", "second", EventAttemptOutcome::NotAdopted, None);
        let Some(Poll::Ready {
            cursor,
            more: true,
            attempt_ends,
            ..
        }) = clock.poll(0, &[], true, false, false)
        else {
            panic!("first page");
        };
        assert!(attempt_ends.is_empty());
        let Some(Poll::Ready { attempt_ends, .. }) =
            clock.poll(cursor, &["t".into()], true, false, false)
        else {
            panic!("second page");
        };
        assert_eq!(
            attempt_ends
                .iter()
                .map(|end| end.attempt_id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(attempt_ends[0].seq < attempt_ends[1].seq);
        for n in 0..MAX_ATTEMPT_ENDS {
            clock.record_attempt_end(
                "t",
                &format!("a-{n}"),
                EventAttemptOutcome::NotAdopted,
                None,
            );
        }
        assert!(matches!(
            clock.poll(cursor, &[], true, false, false),
            Some(Poll::Resync { .. })
        ));
        assert!(matches!(
            clock.poll(cursor, &[], false, false, false),
            Some(Poll::Ready { .. })
        ));
    }

    #[test]
    fn index_and_retention_marks_are_independent_and_ignore_the_thread_filter() {
        let clock = ChangeClock::new(Wakes::default());
        clock.record_mark(Mark::Index);
        clock.record("t");
        let Some(Poll::Ready {
            index_changed,
            retention_changed,
            config_changed,
            cursor,
            ..
        }) = clock.poll(0, &["other".into()], false, false, false)
        else {
            panic!("expected ready");
        };
        assert!(index_changed && !retention_changed && !config_changed);
        clock.record_mark(Mark::Retention);
        let Some(Poll::Ready {
            index_changed,
            retention_changed,
            ..
        }) = clock.poll(cursor, &[], false, false, false)
        else {
            panic!("expected ready");
        };
        assert!(!index_changed && retention_changed);
        let mut seen = 0;
        assert!(clock.take_mark(Mark::Retention, &mut seen));
        assert!(!clock.take_mark(Mark::Retention, &mut seen));
        assert!(!clock.take_mark(Mark::Index, &mut seen));
    }

    #[test]
    fn output_entries_coalesce_per_execution_page_with_changes_and_honour_the_filter() {
        let clock = ChangeClock::new(Wakes::default());
        clock.record_output("t", "x");
        clock.record_output("u", "y");
        clock.record_output("t", "x");
        let Some(Poll::Ready { output, .. }) = clock.poll(0, &[], false, false, false) else {
            panic!("expected ready");
        };
        assert!(output.is_empty(), "absent unless asked");
        let Some(Poll::Ready { output, cursor, .. }) = clock.poll(0, &[], false, false, true)
        else {
            panic!("expected ready");
        };
        let ids: Vec<_> = output.iter().map(|o| o.execution_id.as_str()).collect();
        assert_eq!(ids, ["y", "x"]);
        assert!(output[0].seq < output[1].seq);
        let Some(Poll::Ready { output, .. }) = clock.poll(0, &["t".into()], false, false, true)
        else {
            panic!("expected ready");
        };
        assert_eq!(output.len(), 1);
        assert_eq!(
            (
                output[0].thread_id.as_str(),
                output[0].execution_id.as_str()
            ),
            ("t", "x")
        );
        for n in 0..=MAX_EVENT_CHANGES {
            clock.record_output("t", &format!("e{n}"));
        }
        clock.record("after-output");
        let Some(Poll::Ready {
            output,
            changes,
            more: true,
            cursor: page,
            ..
        }) = clock.poll(cursor, &[], false, false, true)
        else {
            panic!("a full output page has more");
        };
        assert_eq!(output.len(), MAX_EVENT_CHANGES);
        assert_eq!(page, output.last().unwrap().seq);
        assert!(changes.is_empty(), "a later change waits for its page");
        let Some(Poll::Ready {
            output,
            changes,
            more: false,
            ..
        }) = clock.poll(page, &[], false, false, true)
        else {
            panic!("second page");
        };
        assert_eq!(output.len(), 1);
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn output_eviction_raises_the_shared_floor() {
        let clock = ChangeClock::with_capacity(2);
        clock.record_output("t", "a");
        let cursor = 1;
        clock.record_output("t", "b");
        clock.record_output("t", "c");
        assert!(matches!(
            clock.poll(0, &[], false, false, true),
            Some(Poll::Resync { .. })
        ));
        let Some(Poll::Ready { output, .. }) = clock.poll(cursor, &[], false, false, true) else {
            panic!("a cursor at the evicted entry missed nothing");
        };
        assert_eq!(output.len(), 2);
    }

    #[test]
    fn orders_by_sequence_and_merges_repeated_changes() {
        let clock = ChangeClock::new(Wakes::default());
        for id in ["a", "b", "a", "a", "c"] {
            clock.record(id);
        }
        let (changes, more, cursor) = ready(clock.poll(0, &[], false, false, false));
        assert_eq!(
            changes,
            vec![("b".into(), 2), ("a".into(), 4), ("c".into(), 5)]
        );
        assert!(!more);
        assert_eq!(cursor, 5);
        let (changes, _, _) = ready(clock.poll(2, &[], false, false, false));
        assert_eq!(changes.len(), 2);
        let (none, _, cursor) = ready(clock.poll(5, &[], false, false, false));
        assert!(none.is_empty());
        assert_eq!(cursor, 5);
    }

    #[test]
    fn config_change_hint_is_coalesced_and_visible_to_each_independent_cursor() {
        let clock = ChangeClock::new(Wakes::default());
        clock.record("thread");
        let older = 0;
        let newer = 1;
        clock.record_config_changed();
        clock.record_config_changed();

        for after in [older, newer] {
            let Some(Poll::Ready { config_changed, .. }) =
                clock.poll(after, &[], false, false, false)
            else {
                panic!("expected ready");
            };
            assert!(
                config_changed,
                "cursor {after} must independently see the mark"
            );
        }
        let Some(Poll::Ready { config_changed, .. }) = clock.poll(3, &[], false, false, false)
        else {
            panic!("expected ready");
        };
        assert!(!config_changed, "a cursor after the mark must not see it");
    }

    #[test]
    fn filter_limits_changes_but_cursor_still_reaches_head() {
        let clock = ChangeClock::new(Wakes::default());
        clock.record("a");
        clock.record("b");
        let (changes, _, cursor) = ready(clock.poll(0, &["b".into()], false, false, false));
        assert_eq!(changes, vec![("b".into(), 2)]);
        assert_eq!(cursor, 2);
        let (changes, _, cursor) = ready(clock.poll(0, &["zzz".into()], false, false, false));
        assert!(changes.is_empty());
        assert_eq!(cursor, 2);
    }

    #[test]
    fn page_of_sixty_four_reports_more_and_resumes_without_a_gap() {
        let clock = ChangeClock::new(Wakes::default());
        for i in 0..100 {
            clock.record(&format!("t{i}"));
        }
        let (first, more, cursor) = ready(clock.poll(0, &[], false, false, false));
        assert_eq!(first.len(), MAX_EVENT_CHANGES);
        assert!(more);
        assert_eq!(cursor, 64);
        let (rest, more, cursor) = ready(clock.poll(cursor, &[], false, false, false));
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
            clock.poll(0, &[], false, false, false),
            Some(Poll::Resync { head: 4, .. })
        ));
        // A reader at the floor has already seen everything evicted.
        let (changes, _, _) = ready(clock.poll(1, &[], false, false, false));
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
        let (changes, _, _) = ready(clock.poll(0, &[], false, false, false));
        assert_eq!(changes.len(), 2);
    }

    fn text_poll(clock: &ChangeClock, after: u64, filter: &[String]) -> (Vec<EventLiveText>, u64) {
        match clock.poll(after, filter, true, false, false) {
            Some(Poll::Ready { text, cursor, .. }) => (text, cursor),
            _ => panic!("expected ready"),
        }
    }
    fn charged(clock: &ChangeClock, thread: &str) -> usize {
        clock.lock().text[thread].charged
    }

    #[test]
    fn text_shares_the_sequence_without_becoming_a_change() {
        let clock = ChangeClock::new(Wakes::default());
        clock.record("t");
        clock.record_text("t", "a1", 0, "he", false);
        clock.record_text("t", "a1", 2, "llo", false);
        clock.record_text("t", "a1", 5, "hm", true);
        clock.record_text("t", "a1", 7, "m", true);
        clock.record_text("t", "a1", 8, "m", true);
        clock.record_text("t", "a1", 9, "!", false);
        clock.record_text("t", "a1", 10, "", false);
        // Plain poll: no text, one change, cursor past the text.
        let (changes, _, cursor) = ready(clock.poll(1, &[], false, false, false));
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
    fn receive_age_tracks_keepalives_without_text_or_retired_attempts() {
        let clock = ChangeClock::new(Wakes::default());
        let first = clock.live_text_guard("selected", "first");
        let baseline = clock.snapshot(&[], true, false).0;
        let now = std::time::Instant::now();
        first.received(now - std::time::Duration::from_secs(30));
        first.received(now - std::time::Duration::from_secs(5));
        let Some(Poll::Ready { text, cursor, .. }) =
            clock.poll(baseline, &["selected".into()], true, false, false)
        else {
            panic!("expected a receive update")
        };
        assert_eq!(cursor, baseline);
        assert_eq!(text[0].attempt_id, "first");
        assert!(text[0].deltas.is_empty());
        assert!(text[0].receive_age_ms.unwrap() >= 5_000);
        assert_eq!(
            clock.lock().reasoning_activity["selected"]
                .last_receive
                .unwrap(),
            now - std::time::Duration::from_secs(5)
        );
        let (hidden, _) = text_poll(&clock, baseline, &["other".into()]);
        assert!(hidden.is_empty());
        let second = clock.live_text_guard("selected", "second");
        first.received(now);
        let (retired, _) = text_poll(&clock, cursor, &["selected".into()]);
        assert!(retired.is_empty());
        second.received(now);
        let (current, _) = text_poll(&clock, cursor, &["selected".into()]);
        assert_eq!(current[0].attempt_id, "second");
    }

    #[test]
    fn recovery_transitions_page_without_coalescing_or_replay() {
        let clock = ChangeClock::with_capacity(MAX_EVENT_CHANGES * 2);
        for _ in 0..MAX_EVENT_CHANGES + 1 {
            clock.record_change("recovering", true);
        }
        clock.record("recovering");
        let Some(Poll::Ready {
            changes,
            more,
            cursor,
            recovery_transitions,
            ..
        }) = clock.poll(0, &[], false, false, false)
        else {
            panic!("expected first recovery page")
        };
        assert!(more);
        assert!(changes.is_empty());
        assert_eq!(recovery_transitions.len(), MAX_EVENT_CHANGES);
        assert_eq!(recovery_transitions.last().unwrap().seq, cursor);
        let Some(Poll::Ready {
            changes,
            more,
            cursor,
            recovery_transitions,
            ..
        }) = clock.poll(cursor, &[], false, false, false)
        else {
            panic!("expected final recovery page")
        };
        assert!(!more);
        assert_eq!(changes.len(), 1);
        assert_eq!(recovery_transitions.len(), 1);
        assert!(recovery_transitions[0].seq < cursor);
        assert!(
            matches!(clock.poll(cursor, &[], false, false, false), Some(Poll::Ready { recovery_transitions, .. }) if recovery_transitions.is_empty())
        );
        let narrow = ChangeClock::with_capacity(1);
        narrow.record_change("recovering", true);
        narrow.record_change("recovering", true);
        assert!(matches!(
            narrow.poll(0, &[], false, false, false),
            Some(Poll::Resync { .. })
        ));
    }

    #[test]
    fn reasoning_activity_reports_attempt_start_elapsed_and_observed_bytes_only() {
        let clock = ChangeClock::new(Wakes::default());
        let guard = clock.live_text_guard("thread", "attempt");
        let Some(Poll::Ready {
            cursor: start_cursor,
            reasoning_activity,
            reasoning_changed,
            ..
        }) = clock.poll(0, &[], false, true, false)
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
        assert!(wire.get("elapsed_ms").is_some());

        // Counts received UTF-8 reasoning bytes, not tokens or retained preview bytes.
        clock.record_text("thread", "attempt", 0, "秘密", true);
        let Some(Poll::Ready {
            reasoning_activity,
            reasoning_changed,
            ..
        }) = clock.poll(start_cursor, &[], false, true, false)
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
                .poll(0, &["other".into()], false, true, false)
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
                .poll(start_cursor, &[], false, true, false)
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
            let clock = ChangeClock::new(Wakes::default());
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
        let clock = ChangeClock::new(Wakes::default());
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
        let clock = ChangeClock::new(Wakes::default());
        let old = clock.live_text_guard("t", "old");
        assert!(old.record(0, "abc", false));
        let current = clock.live_text_guard("t", "new");
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
        let clock = ChangeClock::new(Wakes::default());
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
        drop(clock.live_text_guard("t00", "a"));
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
        let clock = ChangeClock::new(Wakes::default());
        clock.record_text("a", "1", 0, "aa", false);
        clock.record_text("b", "2", 0, "bb", false);
        let (attempts, cursor) = text_poll(&clock, 0, &["b".into()]);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].thread_id, "b");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn a_paged_change_reply_only_returns_text_up_to_its_cursor() {
        let clock = ChangeClock::new(Wakes::default());
        for i in 0..MAX_EVENT_CHANGES {
            clock.record(&format!("t{i}"));
        }
        clock.record_text("attempt-thread", "attempt", 0, "x", false);
        let text_start_seq = clock.lock().text["attempt-thread"].deltas[0].seq;
        let guard = clock.live_text_guard("attempt-thread", "attempt");
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
        }) = clock.poll(0, &[], false, true, false)
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
        }) = clock.poll(cursor, &[], false, true, false)
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
        }) = clock.poll(cursor, &[], false, true, false)
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
        let clock = ChangeClock::new(Wakes::default());
        // Control characters take six JSON bytes each: the worst retained text.
        let piece = "\u{1}".repeat(MAX_ATTEMPT_TEXT_BYTES);
        let id = "i".repeat(128);
        for i in 0..MAX_LIVE_TEXT_ATTEMPTS {
            clock.record_text(&format!("{id}{i}"), &id, 1, &piece, false);
        }
        for i in 0..MAX_EVENT_CHANGES {
            clock.record(&format!("{id}{i}"));
        }
        let Some(Poll::Ready { changes, text, .. }) = clock.poll(0, &[], true, true, false) else {
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
            config_changed: false,
            index_changed: false,
            retention_changed: false,
            live_text: Some(text),
            attempt_ends: None,
            output: None,
            recovery_transitions: vec![],
            reasoning_activity: vec![],
        })
        .unwrap();
        assert!(reply.len() <= MAX_FRAME_BYTES, "{}", reply.len());
        // Largest charged state, delta records included, stays under the stated total.
        let total: usize = clock.lock().text.values().map(|t| t.charged).sum();
        assert!(total <= MAX_LIVE_TEXT_ATTEMPTS * MAX_ATTEMPT_TEXT_BYTES);
    }

    #[tokio::test]
    async fn registered_events_wait_disconnects_and_drains_without_a_receipt() {
        use crate::protocol::{Command, EventsWaitArgs, Request};
        use tokio_util::sync::CancellationToken;
        for include_text in [false, true] {
            let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
            let command = |after| {
                Command::EventsWait(EventsWaitArgs {
                    after,
                    thread_ids: vec![],
                    timeout_ms: 600_000,
                    include_text,
                    include_reasoning_activity: false,
                })
            };
            let baseline = control
                .request(Request::new(
                    control.store.operation_epoch.mint(),
                    command(None),
                ))
                .await;
            let at = baseline.result.unwrap()["cursor"]
                .as_str()
                .unwrap()
                .to_owned();
            for disconnect in [true, false] {
                assert!(control.waits.keyed.is_empty());
                let token = CancellationToken::new();
                let poll = tokio::spawn({
                    let control = control.clone();
                    let token = token.clone();
                    let request = Request::new(
                        control.store.operation_epoch.mint(),
                        command(Some(at.clone())),
                    );
                    async move { control.request_cancellable(request, token).await }
                });
                crate::test_watchdog::watchdog("events.wait keyed registration", async {
                    while control.waits.keyed.is_empty() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let receipts: i64 = control.store.database.call(|db| Ok(db.query_row(
                    "SELECT (SELECT count(*) FROM operation_receipt)+(SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%')", [], |row| row.get(0),
                )?)).await.unwrap();
                assert_eq!(receipts, 0);
                if disconnect {
                    token.cancel();
                } else {
                    control.shutdown().await.unwrap();
                }
                let response =
                    crate::test_watchdog::watchdog("registered events.wait terminates", poll)
                        .await
                        .unwrap()
                        .unwrap();
                assert!(!response.ok);
                assert!(control.waits.keyed.is_empty());
            }
            server.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
        }
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
                include_output: false,
            })
        };
        let changed = control.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let subscription = control.waits.keyed.subscribe([Key::Live(None)]);
        let text = subscription.notify.notified();
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
    async fn events_wait_samples_receive_age_without_completing_for_bytes_alone() {
        use crate::protocol::{Command, EventsWaitArgs, Request};
        let (_root, owner, control, server) = crate::frontend::control_tests::fixture().await;
        let guard = control.fixture_live_text("t");
        let request = |after| {
            Request::new(
                uuid::Uuid::new_v4().to_string(),
                Command::EventsWait(EventsWaitArgs {
                    after,
                    thread_ids: vec!["t".into()],
                    timeout_ms: 0,
                    include_text: true,
                    include_reasoning_activity: false,
                    include_output: false,
                }),
            )
        };
        let baseline = control.request(request(None)).await.result.unwrap();
        let cursor = baseline["cursor"].as_str().unwrap().to_owned();
        guard.received(std::time::Instant::now());
        let idle = control
            .request(request(Some(cursor.clone())))
            .await
            .result
            .unwrap();
        assert_eq!(idle["status"], "timeout");
        assert_eq!(idle["cursor"], cursor);
        assert_eq!(idle["live_text"][0]["attempt_id"], "fixture-attempt");
        assert!(idle["live_text"][0]["receive_age_ms"].is_number());
        assert_eq!(idle["live_text"][0]["deltas"], serde_json::json!([]));
        assert!(guard.record(0, "answer", false));
        let text = control.request(request(Some(cursor))).await.result.unwrap();
        assert_eq!(text["status"], "text");
        assert_eq!(text["live_text"][0]["deltas"][0]["text"], "answer");
        assert!(text["live_text"][0]["receive_age_ms"].is_number());
        drop(guard);
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
                    include_output: false,
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
                    include_output: false,
                }),
            )
        };
        let baseline_response = control.request(wait(None)).await;
        assert!(baseline_response.ok, "{:?}", baseline_response.error);
        let baseline = baseline_response.result.unwrap();
        let cursor = baseline["cursor"].as_str().unwrap().to_owned();
        let guard = control.change_clock.live_text_guard("t", "attempt");
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
                    include_output: false,
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
                                    include_output: false,
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
        let clock = ChangeClock::new(Wakes::default());
        clock.record("a");
        assert!(clock.poll(2, &[], false, false, false).is_none());
    }
}

#[cfg(test)]
mod admission_fence_tests {
    use super::*;
    #[test]
    fn admission_without_text_keeps_text_baseline_empty_but_reasoning_active() {
        let clock = ChangeClock::new(Wakes::default());
        let guard = clock.live_text_guard("t", "a");
        let before = clock.snapshot(&[], true, true);
        assert!(before.1.is_empty());
        assert_eq!(before.2.len(), 1);
        assert!(guard.record(0, "first", false));
        assert_eq!(clock.snapshot(&[], true, true).1.len(), 1);
    }
    #[test]
    fn admitted_newcomer_at_capacity_has_no_live_publication_authority() {
        let clock = ChangeClock::new(Wakes::default());
        let ids = (0..17).map(|i| format!("t{i}")).collect::<Vec<_>>();
        let guards = ids
            .iter()
            .map(|id| clock.live_text_guard(id, id))
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
        let clock = ChangeClock::new(Wakes::default());
        let guard = clock.live_text_guard("t", "a");
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
        let clock = ChangeClock::new(Wakes::default());
        let old = clock.live_text_guard("t", "a");
        let current = clock.live_text_guard("t", "a");
        assert!(!old.record(0, "old", false));
        drop(old);
        assert!(current.record(0, "current", false));
        assert_eq!(
            clock.snapshot(&[], true, true).1[0].deltas[0].text,
            "current"
        );
        let next = clock.live_text_guard("t", "b");
        drop(current);
        assert!(next.record(0, "next", false));
    }
    #[test]
    fn abandoned_admission_and_evicted_guard_cannot_recreate_preview() {
        let clock = ChangeClock::new(Wakes::default());
        let guard = clock.reserve_live("t", "a");
        let (token, alive) = guard.reservation();
        drop(guard);
        clock.admit_live("t", "a", &token, &alive, None);
        assert!(clock.snapshot(&[], true, true).1.is_empty());
        let guard = clock.live_text_guard("t", "a");
        {
            clock.lock().text.remove("t");
        }
        assert!(!guard.record(0, "after eviction", false));
    }
    #[test]
    fn bounded_admission_and_other_thread_survive_exact_revoke() {
        let clock = ChangeClock::new(Wakes::default());
        let ids = (0..17).map(|i| format!("t{i}")).collect::<Vec<_>>();
        let guards = ids
            .iter()
            .map(|id| clock.live_text_guard(id, id))
            .collect::<Vec<_>>();
        assert_eq!(clock.snapshot(&[], true, true).2.len(), 16);
        assert!(!guards[16].record(0, "over cap", true));
        clock.revoke_live("t0", "t0", None);
        assert!(guards[1].record(0, "unaffected", true));
        assert!(!guards[16].record(0, "not readmitted", false));
    }
}
