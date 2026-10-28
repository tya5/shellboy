//! Incremental model source window; independent of UI queue and persistence.
use crate::protocol::MAX_BODY_BYTES;

#[derive(Default)]
pub(super) struct StreamBody {
    pub(super) text: String,
    pub(super) base: usize,
    pub(super) total: usize,
    pub(super) reasoning_ranges: Vec<(usize, usize)>,
    dirty: bool,
    revision: u64,
    gap: bool,
}
impl StreamBody {
    pub(super) fn append(&mut self, text: &str, reasoning: bool) {
        let start = self.total;
        self.text.push_str(text);
        self.total += text.chars().count();
        if reasoning && self.total > start {
            if let Some(last) = self.reasoning_ranges.last_mut()
                && last.1 == start
            {
                last.1 = self.total;
            } else {
                self.reasoning_ranges.push((start, self.total));
            }
        }
        if self.text.len() > MAX_BODY_BYTES as usize {
            let start = self
                .text
                .ceil_char_boundary(self.text.len() - MAX_BODY_BYTES as usize);
            self.base += self.text[..start].chars().count();
            self.text.drain(..start);
            if self.text.capacity() > MAX_BODY_BYTES as usize * 2 {
                self.text.shrink_to(MAX_BODY_BYTES as usize);
            }
            self.reasoning_ranges.retain(|range| range.1 > self.base);
            if self.reasoning_ranges.capacity()
                > self.reasoning_ranges.len().saturating_mul(2).max(128)
            {
                self.reasoning_ranges.shrink_to_fit();
            }
        }
    }
}

use super::{
    ApiError, Control, UiNode, UiUpdate, checked_storage, presentation_work, storage_error,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        mpsc::{SyncSender, TrySendError},
    },
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub(super) struct Ticket {
    pub thread: String,
    pub attempt: String,
    pub generation: u64,
    serial: u64,
    revision: u64,
    pub width: u16,
}
struct Window {
    thread: String,
    attempt: String,
    body: StreamBody,
    serial: u64,
}
#[derive(Default)]
pub(super) struct Display {
    windows: VecDeque<Window>,
    serial: u64,
    #[cfg(test)]
    pub snapshots: usize,
    #[cfg(test)]
    pub cloned_bytes: usize,
}
pub(super) struct Prepared {
    pub ticket: Ticket,
    gap: bool,
    node: Option<UiNode>,
    prepared: bool,
}
pub(super) enum Outcome {
    Queued { ended: bool },
    Full(Box<Prepared>),
    Cancelled,
    Closed,
    Unchanged,
}
impl Display {
    pub fn clear(&mut self) {
        self.windows.clear();
    }
    pub fn resume(&mut self, thread: String, sources: Vec<UiNode>) {
        // Selection transfers only the latest two known source windows, never
        // rendered metadata. Later primary offsets detect offscreen gaps.
        for source in sources.into_iter().take(2).rev() {
            self.serial = self.serial.wrapping_add(1);
            let gap = source.presentation.provisional_stream
                && source.presentation.body_extent_unknown
                || !source.text.is_empty()
                    && source
                        .base_char_offset
                        .saturating_add(source.text.chars().count())
                        != source.total_chars;
            let mut body = StreamBody {
                text: source.text,
                base: source.base_char_offset,
                total: source.total_chars,
                reasoning_ranges: source.reasoning_ranges,
                gap,
                dirty: true,
                revision: 0,
            };
            if body.text.is_empty() {
                body.base = body.total;
                body.reasoning_ranges = Vec::new();
            }
            body.append("", false); // Enforce the same UTF-8 window bound on handoff.
            self.windows.push_back(Window {
                thread: thread.clone(),
                attempt: source.node_id,
                serial: self.serial,
                body,
            });
        }
    }
    pub fn observe(
        &mut self,
        thread: String,
        attempt: String,
        offset: usize,
        text: &str,
        reasoning: bool,
    ) {
        let index = match self
            .windows
            .iter()
            .position(|w| w.thread == thread && w.attempt == attempt)
        {
            Some(index) => index,
            None => {
                // Only the current/previous attempt's preview windows are useful.
                // Durable history is independent of these two disposable slots.
                if self.windows.len() == 2 {
                    self.windows.pop_front();
                }
                self.serial = self.serial.wrapping_add(1);
                self.windows.push_back(Window {
                    thread,
                    attempt,
                    body: StreamBody::default(),
                    serial: self.serial,
                });
                self.windows.len() - 1
            }
        };
        let body = &mut self.windows[index].body;
        if body.gap || offset < body.total {
            return;
        }
        if offset != body.total {
            if !body.text.is_empty() {
                body.gap = true;
                body.dirty = true;
                body.revision = body.revision.wrapping_add(1);
                return;
            }
            body.base = offset;
            body.total = offset;
        }
        if text.is_empty() {
            return;
        }
        body.append(text, reasoning);
        body.revision = body.revision.wrapping_add(1);
        body.dirty = true;
    }
    pub fn invalidate(&mut self, thread: &str) {
        for window in self.windows.iter_mut().filter(|w| w.thread == thread) {
            window.body.dirty = true;
        }
    }
    pub fn pending(&self) -> bool {
        self.windows.iter().any(|w| w.body.dirty)
    }
    pub fn current(&self, ticket: &Ticket, width: u16) -> bool {
        ticket.width == width
            && self.windows.iter().any(|w| {
                w.thread == ticket.thread
                    && w.attempt == ticket.attempt
                    && w.serial == ticket.serial
                    && w.body.revision == ticket.revision
            })
    }
    pub fn remove(&mut self, ticket: &Ticket) {
        self.windows.retain(|w| {
            !(w.thread == ticket.thread && w.attempt == ticket.attempt && w.serial == ticket.serial)
        });
    }
    pub fn take(&mut self, generation: u64, width: u16) -> Option<Prepared> {
        let window = self.windows.iter_mut().rev().find(|w| w.body.dirty)?;
        window.body.dirty = false;
        #[cfg(test)]
        {
            super::stream_metrics::snapshot(
                window.body.text.len()
                    + window.body.reasoning_ranges.len() * std::mem::size_of::<(usize, usize)>(),
            );
            self.snapshots += 1;
            self.cloned_bytes += window.body.text.len()
                + window.body.reasoning_ranges.len() * std::mem::size_of::<(usize, usize)>();
        }
        Some(Prepared {
            gap: window.body.gap,
            ticket: Ticket {
                thread: window.thread.clone(),
                attempt: window.attempt.clone(),
                generation,
                serial: window.serial,
                revision: window.body.revision,
                width,
            },
            node: Some(UiNode {
                node_id: window.attempt.clone(),
                role: "assistant".into(),
                text: window.body.text.clone(),
                base_char_offset: window.body.base,
                total_chars: window.body.total,
                reasoning_ranges: window.body.reasoning_ranges.clone(),
                presentation: Default::default(),
            }),
            prepared: false,
        })
    }
}
impl Prepared {
    pub fn ended(&self) -> bool {
        self.node.is_none()
    }
    async fn prepare(mut self) -> Result<Self, ApiError> {
        if !self.prepared {
            self = presentation_work(move || {
                if let Some(node) = &mut self.node {
                    node.presentation = crate::presentation::project_stream_window(
                        &node.text,
                        node.base_char_offset,
                        &node.reasoning_ranges,
                        self.ticket.width,
                    );
                    node.presentation.body_extent_unknown |= self.gap;
                    seal_stream_node(node, &self.ticket, self.gap);
                }
                self.prepared = true;
                self
            })
            .await?;
        }
        Ok(self)
    }
    pub async fn deliver(
        self,
        control: &Control,
        updates: SyncSender<UiUpdate>,
        cancel: CancellationToken,
    ) -> Result<Outcome, ApiError> {
        if cancel.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let frame = self.prepare().await?;
        if cancel.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        // The indexed read and nonblocking enqueue share the DB worker order
        // with terminal persistence. Formatting and queue waits never run here.
        let queued = control.store.database.call(move |db| {
            if cancel.is_cancelled() { return Ok(Admission::Cancelled); }
            #[cfg(test)] let _gate = super::stream_metrics::Gate::start();
            let mut frame = frame;
            let running: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM model_attempt a JOIN turn u ON u.id=a.turn_id JOIN run r ON r.id=u.run_id JOIN thread t ON t.id=u.thread_id WHERE a.id=?1 AND u.thread_id=?2 AND a.state='inflight' AND a.generation=u.generation AND a.selection_revision=t.selection_revision AND u.state='running' AND r.state='live' AND t.deleted_at IS NULL)", rusqlite::params![frame.ticket.attempt, frame.ticket.thread], |row| row.get(0))?;
            if running && frame.node.as_ref().is_some_and(|node|node.text.is_empty()) { return Ok(Admission::Unchanged); }
            if !running { frame.node = None; }
            let ended = frame.node.is_none();
            let committed: Option<String> = if ended {
                use rusqlite::OptionalExtension;
                db.query_row("SELECT node_id FROM output_item WHERE attempt_id=?1 AND node_id IS NOT NULL ORDER BY ordinal LIMIT 1", [&frame.ticket.attempt], |row| row.get(0)).optional()?
            } else { None };
            let received_source=if ended && committed.is_none(){super::received::source(db,&frame.ticket.attempt)?.filter(|(_,kind)|kind=="incomplete_response")}else{None};
            let ack = Arc::new(Notify::new());
            let update = match frame.node.take() {
                Some(node) => UiUpdate::Stream { thread_id: frame.ticket.thread.clone(), generation: frame.ticket.generation, node_id: node.node_id, role: node.role, text: node.text, base_char_offset: node.base_char_offset, total_chars: node.total_chars, reasoning_ranges: node.reasoning_ranges, presentation: node.presentation, ack: Some(ack.clone()) },
                None if committed.is_some() => UiUpdate::StreamCommitted { thread_id: frame.ticket.thread.clone(), generation: frame.ticket.generation, attempt_id: frame.ticket.attempt.clone(), node_id: committed.as_ref().unwrap().clone() },
                None => UiUpdate::StreamSettled { thread_id: frame.ticket.thread.clone(), generation: frame.ticket.generation, attempt_id: frame.ticket.attempt.clone(), received_source, ack: Some(ack.clone()) },
            };
            match updates.try_send(update) {
                Ok(()) => { if ended && committed.is_some() { ack.notify_one(); } Ok(Admission::Queued { ended, ack, cancel }) },
                Err(TrySendError::Disconnected(_)) => Ok(Admission::Closed),
                Err(TrySendError::Full(update)) => {
                    if let UiUpdate::Stream { node_id, role, text, base_char_offset, total_chars, reasoning_ranges, presentation, .. } = update {
                        frame.node = Some(UiNode { node_id, role, text, base_char_offset, total_chars, reasoning_ranges, presentation });
                    }
                    Ok(Admission::Full(Box::new(frame)))
                }
            }
        }).await.map_err(storage_error);
        match checked_storage(control, queued).await? {
            Admission::Queued { ended, ack, cancel } => {
                tokio::select! { biased; _ = cancel.cancelled() => Ok(Outcome::Cancelled), _ = ack.notified() => Ok(Outcome::Queued { ended }) }
            }
            Admission::Full(frame) => Ok(Outcome::Full(frame)),
            Admission::Closed => Ok(Outcome::Closed),
            Admission::Cancelled => Ok(Outcome::Cancelled),
            Admission::Unchanged => Ok(Outcome::Unchanged),
        }
    }
}
/// A revision token for this disposable projection, not a content/provenance hash.
/// Serial changes on source handoff, revision on every delta/gap and width on
/// reformatting, avoiding serializing/hash-scanning the retained tail each frame.
fn seal_stream_node(node: &mut UiNode, ticket: &Ticket, gap: bool) {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"shellboy-live-projection-v1\0");
    digest.update(ticket.thread.as_bytes());
    digest.update([0]);
    digest.update(ticket.attempt.as_bytes());
    digest.update([0]);
    digest.update(ticket.serial.to_le_bytes());
    digest.update(ticket.revision.to_le_bytes());
    digest.update(ticket.width.to_le_bytes());
    digest.update([u8::from(gap)]);
    node.presentation.projection_stamp = Some(digest.finalize().into());
}

enum Admission {
    Queued {
        ended: bool,
        ack: Arc<Notify>,
        cancel: CancellationToken,
    },
    Full(Box<Prepared>),
    Cancelled,
    Closed,
    Unchanged,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn frame(display: &mut Display, attempt: &str, text: &str) -> Prepared {
        display.observe("thread".into(), attempt.into(), 0, text, false);
        display.take(7, 80).unwrap()
    }
    #[test]
    fn changed_preserves_unicode_prefix_and_missing_delta_never_becomes_a_false_prefix() {
        let mut display = Display::default();
        display.observe("thread".into(), "a".into(), 0, "考え", true);
        display.invalidate("thread");
        display.observe("thread".into(), "a".into(), 2, "回答", false);
        let first = display.take(7, 80).unwrap().node.unwrap();
        assert_eq!(first.text, "考え回答");
        assert_eq!(first.reasoning_ranges, [(0, 2)]);
        display.observe("thread".into(), "a".into(), 6, "欠落後", false);
        display.observe("thread".into(), "a".into(), 9, "続き", false);
        display.invalidate("thread");
        assert_eq!(display.take(7, 80).unwrap().node.unwrap().text, first.text);
        display.observe("thread".into(), "b".into(), 0, "別attempt", false);
        assert_eq!(
            display.take(7, 80).unwrap().node.unwrap().base_char_offset,
            0
        );
        display.observe("thread".into(), "c".into(), 100, "末尾", false);
        let tail = display.take(7, 80).unwrap().node.unwrap();
        assert_eq!((tail.base_char_offset, tail.total_chars), (100, 102));
        assert_eq!(display.windows.len(), 2);
    }
    #[test]
    fn copying_occurs_only_when_a_pending_frame_is_taken_and_windows_are_bounded() {
        let mut display = Display::default();
        for offset in 0..10_000 {
            display.observe("thread".into(), "a".into(), offset, "字", false);
        }
        assert_eq!((display.snapshots, display.cloned_bytes), (0, 0));
        let first = display.take(0, 80).unwrap();
        assert_eq!((display.snapshots, display.cloned_bytes), (1, 30_000));
        assert!(display.take(0, 80).is_none());
        display.clear();
        let second = frame(&mut display, "a", "new");
        assert!(!display.current(&first.ticket, 80));
        display.remove(&first.ticket);
        assert!(display.current(&second.ticket, 80));
        for attempt in ["b", "c", "d"] {
            display.observe(
                "thread".into(),
                attempt.into(),
                0,
                &"字".repeat(100_000),
                true,
            );
        }
        assert_eq!(display.windows.len(), 2);
        for window in display.windows {
            assert!(window.body.text.len() <= MAX_BODY_BYTES as usize);
            assert_eq!(
                window.body.base + window.body.text.chars().count(),
                window.body.total
            );
        }
    }
    async fn seed(control: &Control) {
        control.store.database.call(|db| {
            db.execute("UPDATE turn SET state='running' WHERE id='target-run'", [])?;
            db.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('a','target-run',0,0,'fixture','fixture-model',0,'inflight','{}','now')", [])?;
            Ok(())
        }).await.unwrap();
    }
    async fn next(incoming: &std::sync::mpsc::Receiver<UiUpdate>) -> UiUpdate {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(update) = incoming.try_recv() {
                    return update;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    fn acknowledge(update: UiUpdate, ended: bool) -> Arc<Notify> {
        match update {
            UiUpdate::Stream {
                ack: Some(ack),
                text,
                ..
            } if !ended => {
                assert_eq!(text, "hello");
                ack
            }
            UiUpdate::StreamSettled { ack: Some(ack), .. } if ended => ack,
            update => panic!("unexpected {update:?}"),
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_normal_queue_returns_owned_frame_and_retry_needs_no_stream_ack() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        updates
            .try_send(UiUpdate::Status {
                thread_id: None,
                text: "ordinary update".into(),
            })
            .unwrap();
        let mut display = Display::default();
        let prepared = frame(&mut display, "a", "hello").prepare().await.unwrap();
        let text_ptr = prepared.node.as_ref().unwrap().text.as_ptr();
        let result = prepared
            .deliver(&control, updates.clone(), CancellationToken::new())
            .await
            .unwrap();
        let Outcome::Full(prepared) = result else {
            panic!("must return Full")
        };
        assert_eq!(prepared.node.as_ref().unwrap().text.as_ptr(), text_ptr);
        assert!(prepared.prepared);
        assert!(matches!(
            incoming.try_recv().unwrap(),
            UiUpdate::Status { .. }
        ));
        let job_control = control.clone();
        let job = tokio::spawn(async move {
            prepared
                .deliver(&job_control, updates, CancellationToken::new())
                .await
        });
        let ack = acknowledge(next(&incoming).await, false);
        assert!(!job.is_finished());
        ack.notify_one();
        assert!(matches!(
            job.await.unwrap().unwrap(),
            Outcome::Queued { ended: false }
        ));
        assert_eq!(display.snapshots, 1);
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_releases_ack_job_and_old_ack_cannot_release_next_frame() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        let mut display = Display::default();
        let prepared = frame(&mut display, "a", "hello");
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let c = control.clone();
        let u = updates.clone();
        let job = tokio::spawn(async move { prepared.deliver(&c, u, token).await });
        let old_ack = acknowledge(next(&incoming).await, false);
        cancel.cancel();
        assert!(matches!(job.await.unwrap().unwrap(), Outcome::Cancelled));
        display.invalidate("thread");
        let prepared = display.take(7, 80).unwrap();
        let c = control.clone();
        let job = tokio::spawn(async move {
            prepared
                .deliver(&c, updates, CancellationToken::new())
                .await
        });
        let ack = acknowledge(next(&incoming).await, false);
        assert!(!Arc::ptr_eq(&ack, &old_ack));
        old_ack.notify_one();
        tokio::task::yield_now().await;
        assert!(!job.is_finished());
        ack.notify_one();
        assert!(matches!(
            job.await.unwrap().unwrap(),
            Outcome::Queued { ended: false }
        ));
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_db_closure_rechecks_selection_cancellation_before_enqueue() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        let mut display = Display::default();
        let prepared = frame(&mut display, "a", "hello").prepare().await.unwrap();
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, blocking) = std::sync::mpsc::channel();
        let barrier = owner
            .db
            .try_call(move |_| {
                let _ = entered.send(());
                blocking.recv().unwrap();
                Ok(())
            })
            .unwrap();
        entering.await.unwrap();
        let cancel = CancellationToken::new();
        let delivery = prepared.deliver(&control, updates, cancel.clone());
        tokio::pin!(delivery);
        // prepare is already complete and the empty DB admission queue has room:
        // this poll enqueues the closure behind the blocked DB operation.
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(delivery.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let fence = owner.db.try_call(|_| Ok(())).unwrap();
        cancel.cancel();
        release.send(()).unwrap();
        barrier.receive().await.unwrap();
        assert!(matches!(delivery.await.unwrap(), Outcome::Cancelled));
        fence.receive().await.unwrap();
        assert!(
            incoming.try_recv().is_err(),
            "cancelled closure must not enqueue a frame; releasing its sender is also valid"
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admitted_frame_precedes_terminal_state_but_later_frames_are_settled() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        let mut display = Display::default();
        let prepared = frame(&mut display, "a", "hello");
        let c = control.clone();
        let u = updates.clone();
        let job =
            tokio::spawn(async move { prepared.deliver(&c, u, CancellationToken::new()).await });
        let ack = acknowledge(next(&incoming).await, false);
        owner
            .db
            .call(|db| {
                db.execute("UPDATE model_attempt SET state='failed' WHERE id='a'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        ack.notify_one();
        assert!(matches!(
            job.await.unwrap().unwrap(),
            Outcome::Queued { ended: false }
        ));
        display.invalidate("thread");
        let prepared = display.take(7, 80).unwrap();
        let c = control.clone();
        let job = tokio::spawn(async move {
            prepared
                .deliver(&c, updates, CancellationToken::new())
                .await
        });
        acknowledge(next(&incoming).await, true).notify_one();
        assert!(matches!(
            job.await.unwrap().unwrap(),
            Outcome::Queued { ended: true }
        ));
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[test]
    fn reselection_transfers_known_source_but_does_not_splice_offscreen_gaps() {
        let known = UiNode {
            node_id: "a".into(),
            text: "考え".into(),
            total_chars: 2,
            reasoning_ranges: vec![(0, 2)],
            ..Default::default()
        };
        let mut display = Display::default();
        display.resume("thread".into(), vec![known.clone()]);
        display.observe("thread".into(), "a".into(), 2, "回答", false);
        assert_eq!(display.take(8, 80).unwrap().node.unwrap().text, "考え回答");
        display.clear();
        display.resume("thread".into(), vec![known]);
        display.observe("thread".into(), "a".into(), 4, "取り落とし後", false);
        let frozen = display.take(9, 80).unwrap();
        assert!(frozen.gap);
        assert_eq!(frozen.node.unwrap().text, "考え");
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resumed_preview_retires_on_terminal_changed_without_any_further_delta() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        let mut display = Display::default();
        display.resume(
            "thread".into(),
            vec![UiNode {
                node_id: "a".into(),
                text: "hello".into(),
                total_chars: 5,
                ..Default::default()
            }],
        );
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        let prepared = display.take(8, 80).unwrap();
        let c = control.clone();
        let u = updates.clone();
        let job =
            tokio::spawn(async move { prepared.deliver(&c, u, CancellationToken::new()).await });
        acknowledge(next(&incoming).await, false).notify_one();
        job.await.unwrap().unwrap();
        owner
            .db
            .call(|db| {
                db.execute(
                    "UPDATE model_attempt SET state='cancelled' WHERE id='a'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        display.invalidate("thread");
        let prepared = display.take(8, 80).unwrap();
        let c = control.clone();
        let job = tokio::spawn(async move {
            prepared
                .deliver(&c, updates, CancellationToken::new())
                .await
        });
        acknowledge(next(&incoming).await, true).notify_one();
        assert!(matches!(
            job.await.unwrap().unwrap(),
            Outcome::Queued { ended: true }
        ));
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resumed_evicted_body_is_not_published_as_empty_source_and_saved_output_uses_identity()
    {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        let mut display = Display::default();
        display.resume(
            "thread".into(),
            vec![UiNode {
                node_id: "a".into(),
                total_chars: 100,
                ..Default::default()
            }],
        );
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        assert!(matches!(
            display
                .take(8, 80)
                .unwrap()
                .deliver(&control, updates.clone(), CancellationToken::new())
                .await
                .unwrap(),
            Outcome::Unchanged
        ));
        assert!(incoming.try_recv().is_err());
        owner.db.call(|db|{db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES('saved','assistant_response','now','{}',2,'fixture')",[])?;db.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('a',0,'message','completed','{}','saved')",[])?;db.execute("UPDATE model_attempt SET state='completed' WHERE id='a'",[])?;Ok(())}).await.unwrap();
        display.invalidate("thread");
        assert!(matches!(
            display
                .take(8, 80)
                .unwrap()
                .deliver(&control, updates, CancellationToken::new())
                .await
                .unwrap(),
            Outcome::Queued { ended: true }
        ));
        assert!(
            matches!(incoming.try_recv().unwrap(),UiUpdate::StreamCommitted{attempt_id,node_id,..}if attempt_id=="a"&&node_id=="saved")
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopped_or_reselected_attempt_is_not_displayed_even_before_driver_cleanup() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        for mutation in [
            "UPDATE run SET state='stopping' WHERE id='target-run'",
            "UPDATE turn SET generation=generation+1 WHERE id='target-run'",
            "UPDATE thread SET selection_revision=selection_revision+1 WHERE id='thread'",
        ] {
            let sql = mutation.to_owned();
            owner
                .db
                .call(move |db| {
                    db.execute("UPDATE run SET state='live' WHERE id='target-run'", [])?;
                    db.execute("UPDATE turn SET generation=0 WHERE id='target-run'", [])?;
                    db.execute(
                        "UPDATE thread SET selection_revision=0 WHERE id='thread'",
                        [],
                    )?;
                    db.execute(&sql, [])?;
                    Ok(())
                })
                .await
                .unwrap();
            let mut display = Display::default();
            let prepared = frame(&mut display, "a", "hello");
            let (updates, incoming) = std::sync::mpsc::sync_channel(1);
            let c = control.clone();
            let job = tokio::spawn(async move {
                prepared
                    .deliver(&c, updates, CancellationToken::new())
                    .await
            });
            acknowledge(next(&incoming).await, true).notify_one();
            assert!(matches!(
                job.await.unwrap().unwrap(),
                Outcome::Queued { ended: true }
            ));
        }
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_persistence_prevents_late_frame_and_closed_ui_does_not_wait_for_ack() {
        let (_root, owner, control, server) = super::super::control_tests::fixture().await;
        seed(&control).await;
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        let mut display = Display::default();
        let prepared = frame(&mut display, "a", "hello");
        owner
            .db
            .call(|db| {
                db.execute(
                    "UPDATE model_attempt SET state='completed' WHERE id='a'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let c = control.clone();
        let u = updates.clone();
        let job =
            tokio::spawn(async move { prepared.deliver(&c, u, CancellationToken::new()).await });
        acknowledge(next(&incoming).await, true).notify_one();
        assert!(matches!(
            job.await.unwrap().unwrap(),
            Outcome::Queued { ended: true }
        ));
        drop(incoming);
        display.invalidate("thread");
        let result = display
            .take(7, 80)
            .unwrap()
            .deliver(&control, updates, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(result, Outcome::Closed));
        assert_eq!(
            owner
                .db
                .call(|db| Ok(db.query_row(
                    "SELECT state FROM run WHERE id='independent-run'",
                    [],
                    |row| row.get::<_, String>(0)
                )?))
                .await
                .unwrap(),
            "live"
        );
        server.shutdown().await.unwrap();
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

/// Compare finite displayed source windows on the existing preparation worker.
/// A unique substring is sufficient only when the whole canonical body is known.
fn corresponding_offset(
    source: &UiNode,
    canonical: &UiNode,
    offset: usize,
) -> Result<usize, &'static str> {
    if canonical.presentation.kind != crate::presentation::PresentationKind::Assistant {
        return Err("確定本文の可用性を確認できない");
    }
    if canonical.base_char_offset != 0
        || canonical.text.chars().count() != canonical.total_chars
        || canonical.presentation.body_extent_unknown
    {
        return Err("確定本文が取得窓の範囲外");
    }
    let source_chars = source.text.chars().count();
    if source.text.is_empty()
        || offset < source.base_char_offset
        || offset > source.base_char_offset.saturating_add(source_chars)
    {
        return Err("生成中の位置が保持窓の範囲外");
    }
    if source.base_char_offset == 0
        && source.total_chars == canonical.total_chars
        && source.text == canonical.text
        && source.reasoning_ranges == canonical.reasoning_ranges
    {
        return Ok(offset);
    }
    let Some(byte) = canonical.text.find(&source.text) else {
        return Err("生成途中と確定本文が異なる");
    };
    let after_first = byte
        + canonical.text[byte..]
            .chars()
            .next()
            .map_or(0, char::len_utf8);
    if canonical.text[after_first..].contains(&source.text) {
        return Err("同じ本文範囲が複数あり対応が不明");
    }
    let start = canonical.text[..byte].chars().count();
    let ranges = |ranges: &[(usize, usize)], base: usize| {
        ranges
            .iter()
            .filter_map(|&(a, b)| {
                let a = a.max(base);
                let b = b.min(base.saturating_add(source_chars));
                (a < b).then_some((a - base, b - base))
            })
            .collect::<Vec<_>>()
    };
    if ranges(&source.reasoning_ranges, source.base_char_offset)
        != ranges(&canonical.reasoning_ranges, start)
    {
        return Err("reasoningと本文の区切りが異なる");
    }
    Ok(start + offset - source.base_char_offset)
}
pub(super) async fn rebase(
    control: &Control,
    updates: &SyncSender<UiUpdate>,
    request: crate::ui::StreamRebase,
    width: u16,
) -> Result<(), ApiError> {
    let canonical = super::HistoryReader::new(control, updates, width)
        .node(request.node_id.clone(), 0)
        .await?;
    let crate::ui::StreamRebase {
        thread_id,
        generation,
        view_revision,
        node_id,
        source,
        anchor,
    } = request;
    let mapped =
        presentation_work(move || corresponding_offset(&source, &canonical, anchor.char_offset))
            .await?;
    let (offset, note) = match mapped {
        Ok(offset) => (offset, "生成中の位置を確定本文と照合しました".to_owned()),
        Err(reason) => (0, format!("{reason}。確定本文の先頭を表示しました")),
    };
    let anchor = crate::ui::ViewAnchor {
        node_id: Some(node_id),
        char_offset: offset,
    };
    super::HistoryReader::new(control, updates, width)
        .rebase(
            &super::WindowReply::new(thread_id.clone(), generation, view_revision),
            anchor.clone(),
        )
        .await?;
    // The reader may relocate an off-path source or the user may have moved
    // since this request. Only its actually accepted anchor may receive notice.
    super::update(
        updates,
        UiUpdate::HistoryRebaseNotice {
            thread_id,
            generation,
            view_revision,
            anchor,
            message: note,
        },
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod anchor_tests {
    use super::*;
    fn source(text: &str) -> UiNode {
        UiNode {
            presentation: crate::presentation::NodePresentation {
                kind: crate::presentation::PresentationKind::Assistant,
                ..Default::default()
            },
            text: text.into(),
            total_chars: text.chars().count(),
            ..Default::default()
        }
    }
    #[test]
    fn exact_whole_body_and_unique_unicode_window_preserve_only_proven_positions() {
        let whole = source("あいうあいう");
        assert_eq!(corresponding_offset(&whole, &whole, 4), Ok(4));
        let canonical = source("前置き\n一意の本文\n末尾");
        let mut tail = source("一意の本文");
        tail.base_char_offset = 100;
        tail.total_chars = 200;
        assert_eq!(corresponding_offset(&tail, &canonical, 102), Ok(6));
        assert!(corresponding_offset(&tail, &canonical, 99).is_err());
        let repeated = source("あああ");
        let mut partial = source("ああ");
        partial.total_chars = 100;
        assert_eq!(
            corresponding_offset(&partial, &repeated, 1),
            Err("同じ本文範囲が複数あり対応が不明")
        );
    }
    #[test]
    fn reasoning_boundaries_different_final_text_and_incomplete_windows_are_not_guessed() {
        let mut streamed = source("考え回答");
        streamed.reasoning_ranges = vec![(0, 2)];
        let mut saved = streamed.clone();
        saved.reasoning_ranges = vec![(0, 3)];
        assert_eq!(
            corresponding_offset(&streamed, &saved, 2),
            Err("reasoningと本文の区切りが異なる")
        );
        saved = source("考え\n回答");
        assert_eq!(
            corresponding_offset(&streamed, &saved, 2),
            Err("生成途中と確定本文が異なる")
        );
        saved = source("別の最終本文");
        assert!(corresponding_offset(&streamed, &saved, 2).is_err());
        saved = streamed.clone();
        saved.total_chars += 1;
        assert_eq!(
            corresponding_offset(&streamed, &saved, 2),
            Err("確定本文が取得窓の範囲外")
        );
    }
}

#[cfg(test)]
mod reasoning_anchor_tests {
    use super::*;
    #[test]
    fn equal_reasoning_body_and_unique_shifted_range_preserve_the_unicode_anchor() {
        let source = UiNode {
            text: "考え回答".into(),
            total_chars: 4,
            reasoning_ranges: vec![(0, 2)],
            ..Default::default()
        };
        let mut canonical = source.clone();
        canonical.presentation.kind = crate::presentation::PresentationKind::Assistant;
        assert_eq!(corresponding_offset(&source, &canonical, 2), Ok(2));
        let source = UiNode {
            base_char_offset: 100,
            total_chars: 200,
            reasoning_ranges: vec![(100, 102)],
            ..source
        };
        canonical.text = "前置き考え回答末尾".into();
        canonical.total_chars = 9;
        canonical.reasoning_ranges = vec![(3, 5)];
        assert_eq!(corresponding_offset(&source, &canonical, 102), Ok(5));
    }
}
