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

use super::{ApiError, Control, UiNode, UiUpdate, UpdateTarget, presentation_work};
use std::{
    collections::VecDeque,
    sync::{Arc, mpsc::TrySendError},
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
    source_gap: bool,
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
    Queued,
    Full(Box<Prepared>),
    Cancelled,
    Closed,
    Unchanged,
}
impl Display {
    pub fn clear(&mut self) {
        self.windows.clear();
    }
    #[cfg(test)]
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
                source_gap: false,
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
                    source_gap: false,
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
    pub fn observe_answer(&mut self, thread: &str, attempt: &str, text: &str, gap: bool) {
        // Provisional answer coordinates exclude all private reasoning bytes.
        let offset = self
            .windows
            .iter()
            .find(|w| w.thread == thread && w.attempt == attempt)
            .map_or(0, |w| w.body.total);
        self.observe(thread.to_owned(), attempt.to_owned(), offset, text, false);
        if gap
            && let Some(window) = self
                .windows
                .iter_mut()
                .find(|w| w.thread == thread && w.attempt == attempt)
        {
            window.source_gap = true;
        }
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
    pub fn retire(&mut self, thread: &str, attempt: &str) -> bool {
        let previous = self.windows.len();
        self.windows
            .retain(|w| w.thread != thread || w.attempt != attempt);
        self.windows.len() != previous
    }
    pub fn take(&mut self, generation: u64, width: u16) -> Option<Prepared> {
        let window = self.windows.iter_mut().rev().find(|w| w.body.dirty)?;
        window.body.dirty = false;
        #[cfg(test)]
        {
            self.snapshots += 1;
            self.cloned_bytes += window.body.text.len()
                + window.body.reasoning_ranges.len() * std::mem::size_of::<(usize, usize)>();
        }
        Some(Prepared {
            gap: window.body.gap || window.source_gap,
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
        updates: impl UpdateTarget + Send + 'static,
        cancel: CancellationToken,
    ) -> Result<Outcome, ApiError> {
        if cancel.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let mut frame = self.prepare().await?;
        if cancel.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        let Some(node) = frame.node.take().filter(|node| !node.text.is_empty()) else {
            return Ok(Outcome::Unchanged);
        };
        let ack = Arc::new(Notify::new());
        let update = UiUpdate::Stream {
            thread_id: frame.ticket.thread.clone(),
            generation: frame.ticket.generation,
            node_id: node.node_id,
            role: node.role,
            text: node.text,
            base_char_offset: node.base_char_offset,
            total_chars: node.total_chars,
            reasoning_ranges: node.reasoning_ranges,
            presentation: node.presentation,
            ack: Some(ack.clone()),
        };
        match updates.try_send(update) {
            Ok(()) => tokio::select! {
                biased;
                _ = cancel.cancelled() => Ok(Outcome::Cancelled),
                _ = ack.notified() => Ok(Outcome::Queued),
            },
            Err(TrySendError::Disconnected(_)) => Ok(Outcome::Closed),
            Err(TrySendError::Full(UiUpdate::Stream {
                node_id,
                role,
                text,
                base_char_offset,
                total_chars,
                reasoning_ranges,
                presentation,
                ..
            })) => {
                frame.node = Some(UiNode {
                    node_id,
                    role,
                    text,
                    base_char_offset,
                    total_chars,
                    reasoning_ranges,
                    presentation,
                });
                Ok(Outcome::Full(Box::new(frame)))
            }
            Err(TrySendError::Full(_)) => unreachable!("delivery returns the original frame"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // A stale ticket must not evict the new source.
        assert_ne!(first.ticket.serial, second.ticket.serial);
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
    async fn next(incoming: &std::sync::mpsc::Receiver<UiUpdate>) -> UiUpdate {
        crate::test_watchdog::watchdog("src/frontend/streaming.rs: next", async {
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
    fn acknowledge(update: UiUpdate) -> Arc<Notify> {
        match update {
            UiUpdate::Stream {
                ack: Some(ack),
                text,
                ..
            } => {
                assert_eq!(text, "hello");
                ack
            }
            update => panic!("unexpected {update:?}"),
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_normal_queue_returns_owned_frame_and_retry_needs_no_stream_ack() {
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
            .deliver(updates.clone(), CancellationToken::new())
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
        let job =
            tokio::spawn(async move { prepared.deliver(updates, CancellationToken::new()).await });
        let ack = acknowledge(next(&incoming).await);
        assert!(!job.is_finished());
        ack.notify_one();
        assert!(matches!(job.await.unwrap().unwrap(), Outcome::Queued));
        assert_eq!(display.snapshots, 1);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_releases_ack_job_and_old_ack_cannot_release_next_frame() {
        let (updates, incoming) = std::sync::mpsc::sync_channel(1);
        let mut display = Display::default();
        let prepared = frame(&mut display, "a", "hello");
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let u = updates.clone();
        let job = tokio::spawn(async move { prepared.deliver(u, token).await });
        let old_ack = acknowledge(next(&incoming).await);
        cancel.cancel();
        assert!(matches!(job.await.unwrap().unwrap(), Outcome::Cancelled));
        display.invalidate("thread");
        let prepared = display.take(7, 80).unwrap();
        let job =
            tokio::spawn(async move { prepared.deliver(updates, CancellationToken::new()).await });
        let ack = acknowledge(next(&incoming).await);
        assert!(!Arc::ptr_eq(&ack, &old_ack));
        old_ack.notify_one();
        tokio::task::yield_now().await;
        assert!(!job.is_finished());
        ack.notify_one();
        assert!(matches!(job.await.unwrap().unwrap(), Outcome::Queued));
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
    #[tokio::test]
    async fn evicted_body_is_not_reissued_as_empty_preview() {
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
        let result = display
            .take(8, 80)
            .unwrap()
            .deliver(updates, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(result, Outcome::Unchanged));
        assert!(incoming.try_recv().is_err());
    }
}

/// Compare finite displayed source windows on the existing preparation worker.
/// A unique substring is sufficient only when the whole canonical body is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RebaseFailure {
    Unavailable,
    CanonicalOutsideWindow,
    SourceOutsideWindow,
    DifferentBody,
    Ambiguous,
    DifferentReasoning,
}
impl RebaseFailure {
    fn message(self, language: crate::config::UiLanguage) -> &'static str {
        use crate::ui::i18n::local;
        match self {
            Self::Unavailable => local!(
                language,
                "確定本文の可用性を確認できない",
                "Cannot verify that the saved body is available"
            ),
            Self::CanonicalOutsideWindow => local!(
                language,
                "確定本文が取得窓の範囲外",
                "The saved body is outside the retrieved window"
            ),
            Self::SourceOutsideWindow => local!(
                language,
                "生成中の位置が保持窓の範囲外",
                "The streaming position is outside the retained window"
            ),
            Self::DifferentBody => local!(
                language,
                "生成途中と確定本文が異なる",
                "The streamed and saved bodies differ"
            ),
            Self::Ambiguous => local!(
                language,
                "同じ本文範囲が複数あり対応が不明",
                "The same body occurs multiple times; correspondence is ambiguous"
            ),
            Self::DifferentReasoning => local!(
                language,
                "reasoningと本文の区切りが異なる",
                "The reasoning and body boundaries differ"
            ),
        }
    }
}
fn corresponding_offset(
    source: &UiNode,
    canonical: &UiNode,
    offset: usize,
) -> Result<usize, RebaseFailure> {
    if canonical.presentation.kind != crate::presentation::PresentationKind::Assistant {
        return Err(RebaseFailure::Unavailable);
    }
    if canonical.base_char_offset != 0
        || canonical.text.chars().count() != canonical.total_chars
        || canonical.presentation.body_extent_unknown
    {
        return Err(RebaseFailure::CanonicalOutsideWindow);
    }
    let source_chars = source.text.chars().count();
    if source.text.is_empty()
        || offset < source.base_char_offset
        || offset > source.base_char_offset.saturating_add(source_chars)
    {
        return Err(RebaseFailure::SourceOutsideWindow);
    }
    if source.base_char_offset == 0
        && source.total_chars == canonical.total_chars
        && source.text == canonical.text
        && source.reasoning_ranges == canonical.reasoning_ranges
    {
        return Ok(offset);
    }
    let Some(byte) = canonical.text.find(&source.text) else {
        return Err(RebaseFailure::DifferentBody);
    };
    let after_first = byte
        + canonical.text[byte..]
            .chars()
            .next()
            .map_or(0, char::len_utf8);
    if canonical.text[after_first..].contains(&source.text) {
        return Err(RebaseFailure::Ambiguous);
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
        return Err(RebaseFailure::DifferentReasoning);
    }
    Ok(start + offset - source.base_char_offset)
}
pub(super) async fn rebase(
    control: &Control,
    updates: &dyn super::UpdateTarget,
    request: crate::ui::StreamRebase,
    width: u16,
) -> Result<(), ApiError> {
    let canonical = super::HistoryReader::new(control, updates, width)
        .node(request.node_id.clone(), 0, &request.thread_id)
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
    let language = control.store.config.ui.language;
    let (offset, note) = match mapped {
        Ok(offset) => (
            offset,
            crate::ui::i18n::local!(
                language,
                "生成中の位置を確定本文と照合しました",
                "Matched the streaming position to the saved body"
            )
            .to_owned(),
        ),
        Err(reason) => (
            0,
            match language {
                crate::config::UiLanguage::Ja => {
                    format!("{}。確定本文の先頭を表示しました", reason.message(language))
                }
                crate::config::UiLanguage::En => format!(
                    "{}. Showing the beginning of the saved body",
                    reason.message(language)
                ),
            },
        ),
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
    fn rebase_failures_are_localized_without_inspecting_user_text() {
        use crate::config::UiLanguage;
        assert_eq!(
            RebaseFailure::DifferentBody.message(UiLanguage::Ja),
            "生成途中と確定本文が異なる"
        );
        assert_eq!(
            RebaseFailure::DifferentBody.message(UiLanguage::En),
            "The streamed and saved bodies differ"
        );
        assert_eq!(
            RebaseFailure::Ambiguous.message(UiLanguage::En),
            "The same body occurs multiple times; correspondence is ambiguous"
        );
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
            Err(RebaseFailure::Ambiguous)
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
            Err(RebaseFailure::DifferentReasoning)
        );
        saved = source("考え\n回答");
        assert_eq!(
            corresponding_offset(&streamed, &saved, 2),
            Err(RebaseFailure::DifferentBody)
        );
        saved = source("別の最終本文");
        assert!(corresponding_offset(&streamed, &saved, 2).is_err());
        saved = streamed.clone();
        saved.total_chars += 1;
        assert_eq!(
            corresponding_offset(&streamed, &saved, 2),
            Err(RebaseFailure::CanonicalOutsideWindow)
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
