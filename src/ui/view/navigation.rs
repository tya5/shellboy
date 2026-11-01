//! Conversation navigation ownership. Structural migration: transitions retain their existing behaviour.
use super::*;

mod events;
mod read;
pub(in crate::ui) use events::NavEvent;
pub(in crate::ui) use read::ReadEvent;

#[derive(Debug)]
pub struct Navigation {
    pub(in crate::ui) committed_preview: Option<(String, String)>,
    pub anchor: ViewAnchor,
    mode: FollowMode,
    intent: read::ReadIntent,
    pub view_revision: u64,
    pub(in crate::ui) scroll: VisualScroll,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            committed_preview: None,
            anchor: ViewAnchor::default(),
            mode: FollowMode::Following,
            intent: read::ReadIntent::default(),
            view_revision: 0,
            scroll: VisualScroll::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::ui) struct VisualScroll {
    position: ProjectionPosition,
    pub(in crate::ui) within: usize,
    pub(in crate::ui) anchor: ViewAnchor,
    pub(in crate::ui) width: u16,
    pub(in crate::ui) height: u16,
    pub(in crate::ui) revision: u64,
    pub(in crate::ui) focus: Option<String>,
    pub(in crate::ui) origin: usize,
    pub(in crate::ui) area: Option<Rect>,
}
impl Default for VisualScroll {
    fn default() -> Self {
        Self {
            position: ProjectionPosition::Stale(0),
            within: 0,
            anchor: ViewAnchor::default(),
            width: 78,
            height: 18,
            revision: 0,
            focus: None,
            origin: 0,
            area: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FollowMode {
    Following,
    FollowingUnseen,
    Reading,
    ReadingUnseen,
}

type ReadTicket = (u64, Option<String>);

impl Navigation {
    pub(in crate::ui) fn pending(&self) -> isize {
        self.intent.pending()
    }
    pub(in crate::ui) fn moved_during_read(&self) -> bool {
        self.intent.moved()
    }
    pub(in crate::ui) fn is_rebasing(&self) -> bool {
        self.intent.rebasing()
    }
    #[cfg(test)]
    pub(in crate::ui) fn seed_pending(&mut self, pending: isize) {
        self.on_event(NavEvent::Read(ReadEvent::SeedPending(pending)));
    }
    #[cfg(test)]
    pub(in crate::ui) fn seed_rebasing(&mut self, rebasing: bool) {
        self.on_event(NavEvent::Read(ReadEvent::SeedRebasing(rebasing)));
    }

    pub fn follow(&self) -> bool {
        matches!(
            self.mode,
            FollowMode::Following | FollowMode::FollowingUnseen
        )
    }
    pub fn unseen(&self) -> bool {
        matches!(
            self.mode,
            FollowMode::FollowingUnseen | FollowMode::ReadingUnseen
        )
    }
    pub fn set_follow(&mut self, follow: bool) {
        self.on_event(NavEvent::FollowingSelected(follow));
    }
    pub fn set_unseen(&mut self, unseen: bool) {
        self.on_event(NavEvent::UnseenSelected(unseen));
    }

    pub fn is_loading(&self) -> bool {
        self.intent.is_loading()
    }
    pub(in crate::ui) fn request(&self) -> Option<&ReadTicket> {
        self.intent.request()
    }
    pub fn set_loading(&mut self, loading: bool) {
        self.on_event(NavEvent::Read(if loading {
            ReadEvent::WaitStarted
        } else {
            ReadEvent::DeliveryObserved
        }));
    }
    #[cfg(test)]
    pub(in crate::ui) fn set_request(&mut self, ticket: Option<ReadTicket>) {
        self.on_event(NavEvent::Read(ReadEvent::SeedTicket(ticket)));
    }
    #[cfg(test)]
    pub(in crate::ui) fn take_request(&mut self) -> Option<ReadTicket> {
        self.on_event(NavEvent::Read(ReadEvent::TakeOwned)).ticket
    }
}

enum BoundaryProgress {
    Clear,
    Advance,
    Actions(Vec<UiAction>),
}

impl ThreadView {
    fn traverse_boundary(
        &mut self,
        boundary: Option<bool>,
        layout: &HistoryLayout,
        amount: isize,
        origin: ViewAnchor,
        within: usize,
        top: usize,
    ) -> BoundaryProgress {
        let view = self;
        if let Some(forward) = boundary
            && let Some(thread_id) = view.thread_id.clone()
        {
            let source = if forward {
                layout.sources.last()
            } else {
                layout.sources.first()
            };
            if let Some(source) = source {
                let node = &view.nodes[source.node];
                let cached_end = node.base_char_offset + node.text.chars().count();
                if !view.closed_tool_source(node)
                    && ((!forward && node.base_char_offset > 0)
                        || (forward && cached_end < node.total_chars))
                {
                    view.navigation.on_event(NavEvent::ReadRequested {
                        ticket: (view.navigation.view_revision, Some(node.node_id.clone())),
                        pending: Some(amount),
                        new_epoch: false,
                    });
                    view.navigation.anchor = origin.clone();
                    view.navigation.scroll.anchor = origin.clone();
                    view.navigation.scroll.within = within;
                    return BoundaryProgress::Actions(vec![UiAction::LoadNodeWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.navigation.view_revision,
                        node_id: node.node_id.clone(),
                        char_offset: if forward {
                            origin.char_offset
                        } else {
                            // frontend::window starts 512 Unicode characters
                            // before its requested offset. Ask for 8192 total
                            // lookbehind (at most 32KiB), not one 513-char step.
                            node.base_char_offset.saturating_sub(8192 - 512)
                        },
                    }]);
                }
            }
            if let Some(source) = source {
                let adjacent = if forward {
                    (source.covered_end < view.nodes.len()).then_some(source.covered_end)
                } else {
                    // Quiet observations may be painted after later prose.
                    // Their canonical prefix still belongs to this bounded
                    // projection; navigation uses its range, not row order.
                    presentation::projection_bounds(view).0.checked_sub(1)
                };
                if let Some(index) = adjacent {
                    let focus = projection_node_index(view);
                    if (forward && index <= focus) || (!forward && index >= focus) {
                        return BoundaryProgress::Actions(vec![]);
                    }
                    view.navigation.scroll.focus = Some(view.nodes[index].node_id.clone());
                    view.navigation.anchor = origin.clone();
                    view.navigation.scroll.anchor = origin;
                    view.navigation.scroll.within = within;
                    view.navigation.on_event(NavEvent::SourceInvalidated);
                    return BoundaryProgress::Advance;
                }
            }
            let ordinal = if forward {
                view.first_ordinal + view.selected_node_count() as u64
            } else {
                view.first_ordinal.saturating_sub(1)
            };
            if (forward && ordinal < view.total_nodes) || (!forward && view.first_ordinal > 0) {
                // Keep the last visible frame while metadata is in flight.
                // Its source range is retained when the overlapping page arrives.
                view.navigation.on_event(NavEvent::PositionResolved {
                    position: Some((origin, within)),
                    row: top,
                    follow_tail: false,
                    local_read: false,
                });
                view.navigation.on_event(NavEvent::ReadRequested {
                    ticket: (view.navigation.view_revision, None),
                    pending: Some(amount),
                    new_epoch: false,
                });
                view.navigation.scroll.anchor = view.navigation.anchor.clone();
                return BoundaryProgress::Actions(vec![UiAction::LoadHistoryWindow {
                    thread_id,
                    generation: view.generation,
                    view_revision: view.navigation.view_revision,
                    ordinal,
                    char_offset: 0,
                    from_end: !forward,
                    selection_revision: view.selection_revision,
                }]);
            }
        }
        BoundaryProgress::Clear
    }

    fn scroll_during_read(
        &mut self,
        amount: isize,
        view_key: &str,
        cache: &mut HistoryLayoutCache,
        budget: usize,
    ) -> Option<Vec<UiAction>> {
        if self.navigation.is_loading() {
            if self.navigation.request().is_some() {
                if self.navigation.pending() != 0 {
                    if self.navigation.pending().signum() == amount.signum() {
                        self.navigation
                            .on_event(NavEvent::Read(ReadEvent::Deferred(amount)));
                        return Some(vec![]);
                    }
                    // Reversing direction cancels navigation that could not
                    // yet be applied, rather than replaying it after the read.
                    self.navigation
                        .on_event(NavEvent::Read(ReadEvent::DeferredDiscarded));
                }
                let (width, height) = (self.navigation.scroll.width, self.navigation.scroll.height);
                let layout = self.history_layout(view_key, cache, budget, width, height);
                let top = self.viewport_row(&layout, width, height);
                let max = layout.lines.saturating_sub(usize::from(height));
                let target = top.saturating_add_signed(amount).min(max.max(top));
                if target != top
                    && let (Some(from), Some(to)) =
                        (layout.sources.get(top), layout.sources.get(target))
                    && let Some(node) = self.nodes.get(to.node)
                    && ((from.node == to.node
                        && !node.text.is_empty()
                        && to.offset >= node.base_char_offset
                        && to.offset < node.base_char_offset + node.text.chars().count())
                        || (from.node.min(to.node)..=from.node.max(to.node)).all(|index| {
                            let node = &self.nodes[index];
                            self.closed_tool_source(node)
                                || (!node.presentation.body_evicted
                                    && node.base_char_offset == 0
                                    && node.text.chars().count() >= node.total_chars)
                        }))
                {
                    let anchor = ViewAnchor {
                        node_id: Some(node.node_id.clone()),
                        char_offset: to.offset,
                    };
                    let first = layout
                        .sources
                        .iter()
                        .position(|r| r.node == to.node && r.offset == to.offset)
                        .unwrap_or(target);
                    self.navigation.on_event(NavEvent::PositionResolved {
                        position: Some((anchor, target.saturating_sub(first))),
                        row: target,
                        follow_tail: false,
                        local_read: true,
                    });
                    return Some(vec![]);
                }
                self.navigation
                    .on_event(NavEvent::Read(ReadEvent::Deferred(amount)));
                return Some(vec![]);
            }
            // Explicit navigation supersedes presentation-only hydration.
            self.navigation.set_loading(false);
        }
        None
    }
    fn hydrate_scroll_anchor(&mut self, amount: isize) -> Option<Vec<UiAction>> {
        if !self.navigation.follow() {
            let view = &mut *self;
            if let Some(node) = view
                .nodes
                .iter()
                .find(|n| Some(&n.node_id) == view.navigation.anchor.node_id.as_ref())
            {
                let offset = view.navigation.anchor.char_offset;
                let end = node.base_char_offset + node.text.chars().count();
                if (offset < node.base_char_offset || (offset >= end && offset < node.total_chars))
                    && !view.closed_tool_source(node)
                    && let Some(thread_id) = view.thread_id.clone()
                {
                    view.navigation.on_event(NavEvent::ReadRequested {
                        ticket: (view.navigation.view_revision, Some(node.node_id.clone())),
                        pending: Some(amount),
                        new_epoch: true,
                    });
                    return Some(vec![UiAction::LoadNodeWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.navigation.view_revision,
                        node_id: node.node_id.clone(),
                        char_offset: offset,
                    }]);
                }
            }
        }
        None
    }
    fn at_true_tail(&self, layout: &HistoryLayout) -> bool {
        layout
            .sources
            .last()
            .is_none_or(|r| r.covered_end == self.nodes.len())
            && self.first_ordinal + self.selected_node_count() as u64 >= self.total_nodes
            && self
                .nodes
                .last()
                .is_none_or(|n| n.base_char_offset + n.text.chars().count() >= n.total_chars)
    }
    pub(in crate::ui) fn viewport_row(
        &mut self,
        layout: &HistoryLayout,
        width: u16,
        height: u16,
    ) -> usize {
        let view = &mut *self;
        let mut row = view.navigation.scroll.row();
        let max = layout.lines.saturating_sub(usize::from(height));
        if view.navigation.follow() {
            row = max;
        } else if !view.navigation.scroll.initialized()
            || view.navigation.scroll.width != width
            || view.navigation.scroll.revision != view.history_revision
            || view.navigation.scroll.anchor != view.navigation.anchor
            || view.navigation.scroll.origin != layout.sources.first().map_or(0, |r| r.node)
        {
            let index = view
                .navigation
                .anchor
                .node_id
                .as_ref()
                .and_then(|id| view.nodes.iter().position(|n| &n.node_id == id))
                .unwrap_or(0);
            let offset = layout
                .sources
                .iter()
                .filter(|r| r.node == index && r.offset <= view.navigation.anchor.char_offset)
                .map(|r| r.offset)
                .max();
            let first = offset
                .and_then(|offset| {
                    layout
                        .sources
                        .iter()
                        .position(|r| r.node == index && r.offset == offset)
                })
                .or_else(|| layout.sources.iter().position(|r| r.node == index))
                .unwrap_or(0);
            let same = view.navigation.scroll.anchor == view.navigation.anchor
                && view.navigation.scroll.width == width;
            let extra = if same {
                view.navigation.scroll.within
            } else {
                0
            };
            row = first
                .saturating_add(extra)
                .min(layout.lines.saturating_sub(1));
        }
        // At a true end the viewport never exposes a mostly blank last page.
        let at_end = view.at_true_tail(layout);
        if at_end {
            row = row.min(max);
        }
        view.navigation.on_event(NavEvent::FrameProjected {
            row,
            width,
            height,
            revision: view.history_revision,
            origin: layout.sources.first().map_or(0, |r| r.node),
        });
        row
    }
    pub(in crate::ui) fn scroll(
        &mut self,
        amount: isize,
        view_key: &str,
        cache: &mut HistoryLayoutCache,
        budget: usize,
    ) -> Vec<UiAction> {
        let amount = if self.navigation.request().is_none() {
            amount.saturating_add(
                self.navigation
                    .on_event(NavEvent::Read(ReadEvent::DeferredDrained))
                    .pending,
            )
        } else {
            amount
        };
        let following = self.navigation.follow();
        if self.navigation.committed_preview.is_some()
            && !self.navigation.is_rebasing()
            && let Some(action) = self.rebase_preview()
        {
            // Replay the initiating movement after the committed stream has a
            // durable anchor, just like movement arriving while the read runs.
            self.navigation
                .on_event(NavEvent::Read(ReadEvent::Deferred(amount)));
            return vec![action];
        }
        // Projection changes must advance through cached nodes without nesting
        // UI calls. The bound and direction check also reject a stalled layout.
        for _ in 0..=self.nodes.len() {
            if self.nodes.is_empty() {
                return vec![];
            }
            if let Some(actions) = self.scroll_during_read(amount, view_key, cache, budget) {
                return actions;
            }
            if let Some(actions) = self.hydrate_scroll_anchor(amount) {
                return actions;
            }
            let (width, height) = (self.navigation.scroll.width, self.navigation.scroll.height);
            let layout = self.history_layout(view_key, cache, budget, width, height);
            let top = self.viewport_row(&layout, width, height);
            if following && amount > 0 && self.at_true_tail(&layout) {
                return vec![];
            }
            let top_source = layout.sources.get(top).cloned();
            let origin = top_source
                .as_ref()
                .map(|source| ViewAnchor {
                    node_id: Some(self.nodes[source.node].node_id.clone()),
                    char_offset: source.offset,
                })
                .unwrap_or_else(|| self.navigation.anchor.clone());
            let within = top_source.as_ref().map_or(0, |source| {
                top.saturating_sub(
                    layout
                        .sources
                        .iter()
                        .position(|r| r.node == source.node && r.offset == source.offset)
                        .unwrap_or(top),
                )
            });
            let target = top as isize + amount;
            let max = layout.lines.saturating_sub(usize::from(height));
            let view = &mut *self;
            view.navigation.on_event(NavEvent::ScrollInitiated);
            if let (Some(from), Some(to)) = (
                top_source,
                layout
                    .sources
                    .get((target.max(0) as usize).min(layout.sources.len().saturating_sub(1))),
            ) {
                // entry=true means the unknown range precedes the cached body in
                // this direction; otherwise it follows the known cached rows.
                let gap = |index: usize| {
                    let node = &view.nodes[index];
                    if view.closed_tool_source(node) {
                        return None;
                    }
                    let empty = node.text.is_empty() && node.total_chars > 0;
                    let end = node.base_char_offset + node.text.chars().count();
                    if empty
                        || (amount < 0 && index < from.node && end < node.total_chars)
                        || (amount > 0 && index > from.node && node.base_char_offset > 0)
                    {
                        Some((index, true, empty))
                    } else if (amount < 0 && index > to.node && node.base_char_offset > 0)
                        || (amount > 0 && index < to.node && end < node.total_chars)
                    {
                        Some((index, false, false))
                    } else {
                        None
                    }
                };
                let missing = if amount < 0 {
                    (to.node..=from.node).rev().find_map(gap)
                } else {
                    (from.node..=to.node).find_map(gap)
                };
                if let Some((index, entry, empty)) = missing
                    && let Some(thread_id) = view.thread_id.clone()
                {
                    let checkpoint = if entry {
                        if amount < 0 {
                            layout.sources.iter().position(|r| r.node > index)
                        } else {
                            layout.sources.iter().rposition(|r| r.node < index)
                        }
                    } else if amount < 0 {
                        layout.sources.iter().position(|r| r.node == index)
                    } else {
                        layout
                            .sources
                            .iter()
                            .rposition(|r| r.node == index)
                            .map(|row| {
                                // The gap continues this same node: its synthetic
                                // separator is not a row of the source continuation.
                                if layout.styled[row].spans.is_empty() {
                                    row.saturating_sub(1)
                                } else {
                                    row
                                }
                            })
                    }
                    .unwrap_or(top);
                    let position = layout.sources.get(checkpoint).map(|source| {
                        let anchor = ViewAnchor {
                            node_id: Some(view.nodes[source.node].node_id.clone()),
                            char_offset: source.offset,
                        };
                        let first = layout
                            .sources
                            .iter()
                            .position(|r| r.node == source.node && r.offset == source.offset)
                            .unwrap_or(checkpoint);
                        (anchor, checkpoint.saturating_sub(first))
                    });
                    view.navigation.on_event(NavEvent::PositionResolved {
                        position,
                        row: checkpoint,
                        follow_tail: false,
                        local_read: false,
                    });
                    let node = &view.nodes[index];
                    view.navigation.on_event(NavEvent::ReadRequested {
                        ticket: (view.navigation.view_revision, Some(node.node_id.clone())),
                        pending: Some(target.saturating_sub(checkpoint as isize)),
                        new_epoch: false,
                    });
                    let offset = if amount < 0 {
                        if entry || empty {
                            node.total_chars.saturating_sub(8192 - 512)
                        } else {
                            node.base_char_offset.saturating_sub(8192 - 512)
                        }
                    } else if entry {
                        0
                    } else {
                        view.navigation.anchor.char_offset
                    };
                    return vec![UiAction::LoadNodeWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.navigation.view_revision,
                        node_id: node.node_id.clone(),
                        char_offset: offset,
                    }];
                }
            }
            let end = view.at_true_tail(&layout);
            let row = target.max(0) as usize;
            let row = if end {
                row.min(max)
            } else {
                row.min(layout.lines.saturating_sub(1))
            };
            let position = layout.sources.get(row).map(|source| {
                let anchor = ViewAnchor {
                    node_id: Some(view.nodes[source.node].node_id.clone()),
                    char_offset: source.offset,
                };
                let first = layout
                    .sources
                    .iter()
                    .position(|r| r.node == source.node && r.offset == source.offset)
                    .unwrap_or(row);
                (anchor, row.saturating_sub(first))
            });
            let follow_tail = amount > 0
                && target >= max as isize
                && end
                && (following || row > top || (max == 0 && view.first_ordinal == 0));
            view.navigation.on_event(NavEvent::PositionResolved {
                position,
                row,
                follow_tail,
                local_read: false,
            });
            let boundary = if target < 0 {
                Some(false)
            } else if amount > 0 && !end && target >= max as isize {
                Some(true)
            } else {
                None
            };
            match view.traverse_boundary(boundary, &layout, amount, origin, within, top) {
                BoundaryProgress::Actions(actions) => return actions,
                BoundaryProgress::Advance => continue,
                BoundaryProgress::Clear => {}
            }
            // A metadata-only row needs its original source window before moving
            // inside that node; no character delta is substituted for row distance.
            if let Some(source) = layout.sources.get(row) {
                let node = &view.nodes[source.node];
                if (node.text.is_empty() && node.total_chars > 0)
                    && !view.closed_tool_source(node)
                    && let Some(thread_id) = view.thread_id.clone()
                {
                    view.navigation.on_event(NavEvent::ReadRequested {
                        ticket: (view.navigation.view_revision, Some(node.node_id.clone())),
                        pending: Some(amount),
                        new_epoch: false,
                    });
                    return vec![UiAction::LoadNodeWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.navigation.view_revision,
                        node_id: node.node_id.clone(),
                        char_offset: view.navigation.anchor.char_offset,
                    }];
                }
            }
            return vec![];
        }
        vec![]
    }
}

#[derive(Clone, Copy, Debug)]
enum ProjectionPosition {
    Stale(usize),
    Current(usize),
}
impl VisualScroll {
    pub(in crate::ui) fn row(&self) -> usize {
        match self.position {
            ProjectionPosition::Stale(row) | ProjectionPosition::Current(row) => row,
        }
    }
    pub(in crate::ui) fn initialized(&self) -> bool {
        matches!(self.position, ProjectionPosition::Current(_))
    }
}
impl FollowMode {
    fn new(follow: bool, unseen: bool) -> Self {
        match (follow, unseen) {
            (true, false) => Self::Following,
            (true, true) => Self::FollowingUnseen,
            (false, false) => Self::Reading,
            (false, true) => Self::ReadingUnseen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_mode_preserves_all_characterized_flag_combinations() {
        for follow in [false, true] {
            for unseen in [false, true] {
                let mut nav = Navigation::default();
                nav.set_follow(follow);
                nav.set_unseen(unseen);
                assert_eq!((nav.follow(), nav.unseen()), (follow, unseen));
                nav.set_follow(!follow);
                assert_eq!((nav.follow(), nav.unseen()), (!follow, unseen));
                nav.set_unseen(!unseen);
                assert_eq!((nav.follow(), nav.unseen()), (!follow, !unseen));
            }
        }
    }

    #[test]
    fn read_state_distinguishes_hydration_awaiting_and_delivered_tickets() {
        for loading in [false, true] {
            for owned in [false, true] {
                let mut nav = Navigation::default();
                let ticket = owned.then(|| (17, Some("node".into())));
                nav.set_loading(loading);
                nav.set_request(ticket.clone());
                assert_eq!(nav.is_loading(), loading);
                assert_eq!(nav.request(), ticket.as_ref());
                nav.set_loading(!loading);
                assert_eq!(nav.is_loading(), !loading);
                assert_eq!(nav.request(), ticket.as_ref());
                assert_eq!(nav.take_request(), ticket);
                assert_eq!(nav.is_loading(), !loading);
                assert!(nav.request().is_none());
            }
        }
    }
}
