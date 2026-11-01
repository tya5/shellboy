//! Conversation navigation ownership. Structural migration: transitions retain their existing behaviour.
use super::*;

#[derive(Debug)]
pub struct Navigation {
    pub(super) committed_preview: Option<(String, String)>,
    pub anchor: ViewAnchor,
    mode: FollowMode,
    read: ReadState,
    pub(super) history_rebase_pending: bool,
    pub view_revision: u64,
    pub(super) scroll: VisualScroll,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            committed_preview: None,
            anchor: ViewAnchor::default(),
            mode: FollowMode::Following,
            read: ReadState::Idle,
            history_rebase_pending: false,
            view_revision: 0,
            scroll: VisualScroll::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct VisualScroll {
    pub(super) row: usize,
    pub(super) within: usize,
    pub(super) anchor: ViewAnchor,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) revision: u64,
    pub(super) pending: isize,
    pub(super) moved_during_read: bool,
    pub(super) focus: Option<String>,
    pub(super) initialized: bool,
    pub(super) origin: usize,
    pub(super) area: Option<Rect>,
}
impl Default for VisualScroll {
    fn default() -> Self {
        Self {
            row: 0,
            within: 0,
            anchor: ViewAnchor::default(),
            width: 78,
            height: 18,
            revision: 0,
            pending: 0,
            moved_during_read: false,
            focus: None,
            initialized: false,
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

#[derive(Debug)]
enum ReadState {
    Idle,
    Hydrating,
    Awaiting(ReadTicket),
    Delivered(ReadTicket),
}
impl ReadState {
    fn new(loading: bool, ticket: Option<ReadTicket>) -> Self {
        match (loading, ticket) {
            (false, None) => Self::Idle,
            (true, None) => Self::Hydrating,
            (true, Some(ticket)) => Self::Awaiting(ticket),
            (false, Some(ticket)) => Self::Delivered(ticket),
        }
    }
}
impl Navigation {
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
        self.set_mode(follow, self.unseen());
    }
    pub fn set_unseen(&mut self, unseen: bool) {
        self.set_mode(self.follow(), unseen);
    }
    fn set_mode(&mut self, follow: bool, unseen: bool) {
        self.mode = match (follow, unseen) {
            (true, false) => FollowMode::Following,
            (true, true) => FollowMode::FollowingUnseen,
            (false, false) => FollowMode::Reading,
            (false, true) => FollowMode::ReadingUnseen,
        };
    }
    pub(super) fn begin_read(&mut self, ticket: ReadTicket, pending: Option<isize>) {
        self.read = ReadState::Awaiting(ticket);
        if let Some(pending) = pending {
            self.scroll.pending = pending;
        }
    }
    pub fn is_loading(&self) -> bool {
        matches!(self.read, ReadState::Hydrating | ReadState::Awaiting(_))
    }
    pub(super) fn request(&self) -> Option<&ReadTicket> {
        match &self.read {
            ReadState::Awaiting(ticket) | ReadState::Delivered(ticket) => Some(ticket),
            ReadState::Idle | ReadState::Hydrating => None,
        }
    }
    pub fn set_loading(&mut self, loading: bool) {
        let ticket = self.take_request();
        self.read = ReadState::new(loading, ticket);
    }
    pub(super) fn set_request(&mut self, ticket: Option<ReadTicket>) {
        self.read = ReadState::new(self.is_loading(), ticket);
    }
    pub(super) fn take_request(&mut self) -> Option<ReadTicket> {
        let empty = ReadState::new(self.is_loading(), None);
        match std::mem::replace(&mut self.read, empty) {
            ReadState::Awaiting(ticket) | ReadState::Delivered(ticket) => Some(ticket),
            ReadState::Idle | ReadState::Hydrating => None,
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

impl ThreadView {
    fn scroll_during_read(
        &mut self,
        amount: isize,
        view_key: &str,
        cache: &mut HistoryLayoutCache,
        budget: usize,
    ) -> Option<Vec<UiAction>> {
        if self.navigation.is_loading() {
            if self.navigation.request().is_some() {
                let mut reversed = false;
                if self.navigation.scroll.pending != 0 {
                    if self.navigation.scroll.pending.signum() == amount.signum() {
                        self.navigation.scroll.pending =
                            self.navigation.scroll.pending.saturating_add(amount);
                        return Some(vec![]);
                    }
                    // Reversing direction cancels navigation that could not
                    // yet be applied, rather than replaying it after the read.
                    self.navigation.scroll.pending = 0;
                    reversed = true;
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
                    self.navigation.anchor = ViewAnchor {
                        node_id: Some(node.node_id.clone()),
                        char_offset: to.offset,
                    };
                    let first = layout
                        .sources
                        .iter()
                        .position(|row| row.node == to.node && row.offset == to.offset)
                        .unwrap_or(target);
                    self.navigation.scroll.within = target.saturating_sub(first);
                    self.navigation.scroll.row = target;
                    self.navigation.scroll.anchor = self.navigation.anchor.clone();
                    self.navigation.scroll.moved_during_read = true;
                    self.navigation.set_follow(false);
                    return Some(vec![]);
                }
                if !reversed {
                    self.navigation.scroll.pending =
                        self.navigation.scroll.pending.saturating_add(amount);
                }
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
                    view.navigation.view_revision = view.navigation.view_revision.wrapping_add(1);
                    view.navigation.begin_read(
                        (view.navigation.view_revision, Some(node.node_id.clone())),
                        Some(amount),
                    );
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
    pub(super) fn viewport_row(
        &mut self,
        layout: &HistoryLayout,
        width: u16,
        height: u16,
    ) -> usize {
        let view = &mut *self;
        let max = layout.lines.saturating_sub(usize::from(height));
        if view.navigation.follow() {
            view.navigation.scroll.row = max;
        } else if !view.navigation.scroll.initialized
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
            view.navigation.scroll.row = first
                .saturating_add(extra)
                .min(layout.lines.saturating_sub(1));
        }
        // At a true end the viewport never exposes a mostly blank last page.
        let at_end = view.at_true_tail(layout);
        if at_end {
            view.navigation.scroll.row = view.navigation.scroll.row.min(max);
        }
        view.navigation.scroll.origin = layout.sources.first().map_or(0, |r| r.node);
        view.navigation.scroll.width = width;
        view.navigation.scroll.height = height;
        view.navigation.scroll.revision = view.history_revision;
        view.navigation.scroll.anchor = view.navigation.anchor.clone();
        view.navigation.scroll.initialized = true;
        view.navigation.scroll.row
    }
    pub(super) fn scroll(
        &mut self,
        amount: isize,
        view_key: &str,
        cache: &mut HistoryLayoutCache,
        budget: usize,
    ) -> Vec<UiAction> {
        let amount = if self.navigation.request().is_none() {
            amount.saturating_add(std::mem::take(&mut self.navigation.scroll.pending))
        } else {
            amount
        };
        let following = self.navigation.follow();
        if self.navigation.committed_preview.is_some()
            && !self.navigation.history_rebase_pending
            && let Some(action) = self.rebase_preview()
        {
            // Replay the initiating movement after the committed stream has a
            // durable anchor, just like movement arriving while the read runs.
            self.navigation.scroll.pending = self.navigation.scroll.pending.saturating_add(amount);
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
            view.navigation.view_revision = view.navigation.view_revision.wrapping_add(1);
            view.navigation.set_follow(false);
            // Empty bodies and missing prefix/suffix ranges have unknown row
            // counts. Stop at the first such range, consume only known rows, and
            // move the source checkpoint beside the next bounded body read.
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
                    if let Some(source) = layout.sources.get(checkpoint) {
                        view.navigation.anchor = ViewAnchor {
                            node_id: Some(view.nodes[source.node].node_id.clone()),
                            char_offset: source.offset,
                        };
                        let first = layout
                            .sources
                            .iter()
                            .position(|r| r.node == source.node && r.offset == source.offset)
                            .unwrap_or(checkpoint);
                        view.navigation.scroll.within = checkpoint.saturating_sub(first);
                    }
                    view.navigation.scroll.row = checkpoint;
                    view.navigation.scroll.anchor = view.navigation.anchor.clone();
                    let node = &view.nodes[index];
                    view.navigation.begin_read(
                        (view.navigation.view_revision, Some(node.node_id.clone())),
                        Some(target.saturating_sub(checkpoint as isize)),
                    );
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
            if let Some(source) = layout.sources.get(row) {
                view.navigation.anchor = ViewAnchor {
                    node_id: Some(view.nodes[source.node].node_id.clone()),
                    char_offset: source.offset,
                };
                let first = layout
                    .sources
                    .iter()
                    .position(|r| r.node == source.node && r.offset == source.offset)
                    .unwrap_or(row);
                view.navigation.scroll.within = row.saturating_sub(first);
            }
            view.navigation.scroll.row = row;
            view.navigation.scroll.anchor = view.navigation.anchor.clone();
            if amount > 0
                && target >= max as isize
                && end
                && (following || row > top || (max == 0 && view.first_ordinal == 0))
            {
                view.navigation.set_follow(true);
                view.navigation.set_unseen(false);
            }
            let boundary = if target < 0 {
                Some(false)
            } else if amount > 0 && !end && target >= max as isize {
                Some(true)
            } else {
                None
            };
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
                        view.navigation.begin_read(
                            (view.navigation.view_revision, Some(node.node_id.clone())),
                            Some(amount),
                        );
                        view.navigation.anchor = origin.clone();
                        view.navigation.scroll.anchor = origin.clone();
                        view.navigation.scroll.within = within;
                        return vec![UiAction::LoadNodeWindow {
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
                        }];
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
                            return vec![];
                        }
                        view.navigation.scroll.focus = Some(view.nodes[index].node_id.clone());
                        view.navigation.anchor = origin.clone();
                        view.navigation.scroll.anchor = origin;
                        view.navigation.scroll.within = within;
                        view.navigation.scroll.initialized = false;
                        continue;
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
                    view.navigation.anchor = origin;
                    view.navigation.scroll.within = within;
                    view.navigation.scroll.row = top;
                    view.navigation
                        .begin_read((view.navigation.view_revision, None), Some(amount));
                    view.navigation.scroll.anchor = view.navigation.anchor.clone();
                    return vec![UiAction::LoadHistoryWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.navigation.view_revision,
                        ordinal,
                        char_offset: 0,
                        from_end: !forward,
                        selection_revision: view.selection_revision,
                    }];
                }
            }
            // A metadata-only row needs its original source window before moving
            // inside that node; no character delta is substituted for row distance.
            if let Some(source) = layout.sources.get(row) {
                let node = &view.nodes[source.node];
                if (node.text.is_empty() && node.total_chars > 0)
                    && !view.closed_tool_source(node)
                    && let Some(thread_id) = view.thread_id.clone()
                {
                    view.navigation.begin_read(
                        (view.navigation.view_revision, Some(node.node_id.clone())),
                        Some(amount),
                    );
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
