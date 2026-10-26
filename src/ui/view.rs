//! One Thread generation's source windows and navigation. No I/O or new workers.
use super::*;
#[derive(Debug)]
pub struct ThreadView {
    pub(super) activity: activity::ActivityView,
    pub thread_id: Option<String>,
    pub cwd: PathBuf,
    pub connection: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub title: String,
    pub state: String,
    pub draft: DraftSnapshot,
    pub generation: u64,
    nodes: Vec<UiNode>,
    pub(super) provisional_nodes: BTreeSet<String>,
    committed_preview: Option<(String, String)>,
    // At most one saved correspondence per retained provisional node.
    committed_nodes: BTreeMap<String, String>,
    pub before_cursor: Option<String>,
    pub first_ordinal: u64,
    pub selection_revision: u64,
    pub total_nodes: u64,
    pub anchor: ViewAnchor,
    pub follow: bool,
    pub unseen: bool,
    pub status: String,
    pub pending: BTreeMap<u64, PendingSend>,
    pub loading: bool,
    pub(super) history_rebase_pending: bool,
    pub(super) tool_refresh: Option<u64>,
    pub(super) tool_refresh_dirty: bool,
    pub save_retry: bool,
    pub(super) draft_restore_pending: bool,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub tool_calls: u64,
    pub cancelled_calls: u64,
    pub view_revision: u64,
    pub reasoning_expanded: BTreeSet<String>,
    history_revision: u64,
    pub(super) scroll: VisualScroll,
    pub(super) composer: composer::Composer,
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
    pub(super) request: Option<(u64, Option<String>)>,
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
            request: None,
            focus: None,
            initialized: false,
            origin: 0,
            area: None,
        }
    }
}
impl ThreadView {
    pub(super) fn observe_sent_nodes(&mut self) {
        for pending in self.pending.values_mut() {
            if let Some(send) = &mut pending.send
                && !send.reflected
                && pending
                    .source_node_id
                    .as_ref()
                    .is_some_and(|id| self.nodes.iter().any(|node| &node.node_id == id))
            {
                send.reflected = true;
            }
        }
        self.pending.retain(|_, pending| {
            !(pending.status == ReceiptStatus::Completed && pending.source_node_id.is_some())
        });
    }
    pub(super) fn sending(&self) -> Option<(activity::ActivityPhase, usize, Instant)> {
        let mut stage = None;
        let mut count = 0;
        let mut started = None;
        for pending in self.pending.values() {
            let Some(send) = &pending.send else { continue };
            let phase = match pending.status {
                ReceiptStatus::Accepting => activity::ActivityPhase::Sending,
                ReceiptStatus::Accepted | ReceiptStatus::Preparing | ReceiptStatus::Completed
                    if !send.reflected =>
                {
                    activity::ActivityPhase::Reflecting
                }
                _ => continue,
            };
            count += 1;
            started = Some(started.map_or(send.started_at, |at: Instant| at.min(send.started_at)));
            if stage.is_none() || phase == activity::ActivityPhase::Sending {
                stage = Some(phase);
            }
        }
        stage.map(|phase| (phase, count, started.expect("a pending send has a clock")))
    }
    pub(super) fn footer_phase(&self) -> activity::ActivityPhase {
        let phase = self.activity.phase();
        if matches!(
            phase,
            activity::ActivityPhase::Stopping
                | activity::ActivityPhase::Recovery
                | activity::ActivityPhase::Failed
        ) {
            return phase;
        }
        self.sending().map_or(phase, |(sending, _, _)| {
            if sending == activity::ActivityPhase::Reflecting && phase.is_active() {
                phase
            } else {
                sending
            }
        })
    }
    pub(super) fn cancel_navigation(&mut self) {
        self.tool_refresh = None;
        self.tool_refresh_dirty = false;
        self.history_rebase_pending = false;
        if self.scroll.request.take().is_some() {
            self.loading = false;
        }
        self.scroll.pending = 0;
        self.view_revision = self.view_revision.wrapping_add(1);
    }
    pub(super) fn request_tool_refresh(&mut self) -> Option<UiAction> {
        if !self.tool_refresh_dirty || self.tool_refresh.is_some() {
            return None;
        }
        self.tool_refresh_dirty = false;
        let thread_id = self.thread_id.clone()?;
        let focus = projection_node_index(self);
        let start = focus.saturating_sub(64);
        let end = (start + 128).min(self.nodes.len());
        let nodes = self.nodes[start..end]
            .iter()
            .filter_map(UiNode::tool_stub)
            .collect::<Vec<_>>();
        if nodes.is_empty() {
            return None;
        }
        let revision = self.history_revision;
        self.tool_refresh = Some(revision);
        Some(UiAction::RefreshTools {
            thread_id,
            generation: self.generation,
            revision,
            nodes,
        })
    }
    pub(super) fn new(config: &Config, cwd: PathBuf) -> Self {
        Self {
            activity: Default::default(),
            thread_id: None,
            cwd,
            connection: config.default_connection.clone(),
            model: config.default_model.clone(),
            effort: config
                .default_connection
                .as_ref()
                .and_then(|name| config.connections.get(name))
                .and_then(|c| c.default_effort.clone()),
            title: "新しい会話".into(),
            state: "idle".into(),
            draft: DraftSnapshot::default(),
            generation: 0,
            nodes: Vec::new(),
            provisional_nodes: BTreeSet::new(),
            committed_preview: None,
            committed_nodes: BTreeMap::new(),
            before_cursor: None,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 0,
            anchor: ViewAnchor::default(),
            follow: true,
            unseen: false,
            status: "入力できます".into(),
            pending: BTreeMap::new(),
            loading: false,
            history_rebase_pending: false,
            tool_refresh: None,
            tool_refresh_dirty: false,
            save_retry: false,
            draft_restore_pending: false,
            run_id: None,
            turn_id: None,
            tool_calls: 0,
            cancelled_calls: 0,
            view_revision: 0,
            reasoning_expanded: BTreeSet::new(),
            history_revision: 0,
            scroll: VisualScroll::default(),
            composer: composer::Composer::default(),
        }
    }
}

impl ThreadView {
    pub(super) fn history_layout(
        &mut self,
        view_key: &str,
        cache: &mut HistoryLayoutCache,
        budget: usize,
        width: u16,
        height: u16,
    ) -> Arc<HistoryLayout> {
        if self.scroll.anchor != self.anchor || self.follow {
            self.scroll.focus = None;
        }
        if self
            .scroll
            .focus
            .as_ref()
            .is_none_or(|id| !self.nodes.iter().any(|n| &n.node_id == id))
        {
            let focus = self
                .nodes
                .get(viewport_node_index(self))
                .map(|node| node.node_id.clone());
            self.scroll.focus = focus;
        }
        cache.evict(budget);
        let key = HistoryLayoutKey::new(view_key, self, width, height);
        cache.discard_old_revision(&key);
        if let Some(value) = cache.get(&key) {
            return value;
        }
        let (styled, targets, sources) = presentation::history(self, width);
        let text = presentation::plain(&styled);
        let lines = Paragraph::new(presentation::borrowed(&styled))
            .wrap(Wrap { trim: false })
            .line_count(width);
        let value = Arc::new(HistoryLayout {
            targets,
            sources,
            text,
            styled,
            lines,
        });
        #[cfg(test)]
        {
            cache.builds += 1;
        }
        cache.insert(key, Arc::clone(&value), budget);
        value
    }
    pub(super) fn viewport_row(
        &mut self,
        layout: &HistoryLayout,
        width: u16,
        height: u16,
    ) -> usize {
        let view = &mut *self;
        let max = layout.lines.saturating_sub(usize::from(height));
        if view.follow {
            view.scroll.row = max;
        } else if !view.scroll.initialized
            || view.scroll.width != width
            || view.scroll.revision != view.history_revision
            || view.scroll.anchor != view.anchor
            || view.scroll.origin != layout.sources.first().map_or(0, |r| r.node)
        {
            let index = view
                .anchor
                .node_id
                .as_ref()
                .and_then(|id| view.nodes.iter().position(|n| &n.node_id == id))
                .unwrap_or(0);
            let offset = layout
                .sources
                .iter()
                .filter(|r| r.node == index && r.offset <= view.anchor.char_offset)
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
            let same = view.scroll.anchor == view.anchor && view.scroll.width == width;
            let extra = if same { view.scroll.within } else { 0 };
            view.scroll.row = first
                .saturating_add(extra)
                .min(layout.lines.saturating_sub(1));
        }
        // At a true end the viewport never exposes a mostly blank last page.
        let at_end = layout
            .sources
            .last()
            .is_none_or(|r| r.covered_end == view.nodes.len())
            && view.first_ordinal + view.nodes.len() as u64 >= view.total_nodes
            && view
                .nodes
                .last()
                .is_none_or(|n| n.base_char_offset + n.text.chars().count() >= n.total_chars);
        if at_end {
            view.scroll.row = view.scroll.row.min(max);
        }
        view.scroll.origin = layout.sources.first().map_or(0, |r| r.node);
        view.scroll.width = width;
        view.scroll.height = height;
        view.scroll.revision = view.history_revision;
        view.scroll.anchor = view.anchor.clone();
        view.scroll.initialized = true;
        view.scroll.row
    }
    pub(super) fn scroll(
        &mut self,
        amount: isize,
        view_key: &str,
        cache: &mut HistoryLayoutCache,
        budget: usize,
    ) -> Vec<UiAction> {
        if self.committed_preview.is_some()
            && !self.history_rebase_pending
            && let Some(action) = self.rebase_preview()
        {
            return vec![action];
        }
        // Projection changes must advance through cached nodes without nesting
        // UI calls. The bound and direction check also reject a stalled layout.
        for _ in 0..=self.nodes.len() {
            if self.nodes.is_empty() {
                return vec![];
            }
            if self.loading {
                let view = &mut *self;
                if view.scroll.request.is_some() {
                    view.scroll.pending = view.scroll.pending.saturating_add(amount);
                }
                return vec![];
            }
            if !self.follow {
                let view = &mut *self;
                if let Some(node) = view
                    .nodes
                    .iter()
                    .find(|n| Some(&n.node_id) == view.anchor.node_id.as_ref())
                {
                    let offset = view.anchor.char_offset;
                    let end = node.base_char_offset + node.text.chars().count();
                    if (offset < node.base_char_offset
                        || (offset >= end && offset < node.total_chars))
                        && let Some(thread_id) = view.thread_id.clone()
                    {
                        view.loading = true;
                        view.scroll.pending = amount;
                        view.view_revision = view.view_revision.wrapping_add(1);
                        view.scroll.request =
                            Some((view.view_revision, Some(node.node_id.clone())));
                        return vec![UiAction::LoadNodeWindow {
                            thread_id,
                            generation: view.generation,
                            view_revision: view.view_revision,
                            node_id: node.node_id.clone(),
                            char_offset: offset,
                        }];
                    }
                }
            }
            let (width, height) = (self.scroll.width, self.scroll.height);
            let layout = self.history_layout(view_key, cache, budget, width, height);
            let top = self.viewport_row(&layout, width, height);
            let top_source = layout.sources.get(top).copied();
            let origin = top_source
                .map(|source| ViewAnchor {
                    node_id: Some(self.nodes[source.node].node_id.clone()),
                    char_offset: source.offset,
                })
                .unwrap_or_else(|| self.anchor.clone());
            let within = top_source.map_or(0, |source| {
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
            view.view_revision = view.view_revision.wrapping_add(1);
            view.follow = false;
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
                        view.anchor = ViewAnchor {
                            node_id: Some(view.nodes[source.node].node_id.clone()),
                            char_offset: source.offset,
                        };
                        let first = layout
                            .sources
                            .iter()
                            .position(|r| r.node == source.node && r.offset == source.offset)
                            .unwrap_or(checkpoint);
                        view.scroll.within = checkpoint.saturating_sub(first);
                    }
                    view.scroll.row = checkpoint;
                    view.scroll.anchor = view.anchor.clone();
                    view.loading = true;
                    view.scroll.pending = target.saturating_sub(checkpoint as isize);
                    let node = &view.nodes[index];
                    view.scroll.request = Some((view.view_revision, Some(node.node_id.clone())));
                    let offset = if amount < 0 {
                        if entry || empty {
                            node.total_chars.saturating_sub(8192 - 512)
                        } else {
                            node.base_char_offset.saturating_sub(8192 - 512)
                        }
                    } else if entry {
                        0
                    } else {
                        view.anchor.char_offset
                    };
                    return vec![UiAction::LoadNodeWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.view_revision,
                        node_id: node.node_id.clone(),
                        char_offset: offset,
                    }];
                }
            }
            let end = layout
                .sources
                .last()
                .is_none_or(|r| r.covered_end == view.nodes.len())
                && view.first_ordinal + view.nodes.len() as u64 >= view.total_nodes
                && view
                    .nodes
                    .last()
                    .is_none_or(|n| n.base_char_offset + n.text.chars().count() >= n.total_chars);
            let row = target.max(0) as usize;
            let row = if end {
                row.min(max)
            } else {
                row.min(layout.lines.saturating_sub(1))
            };
            if let Some(source) = layout.sources.get(row) {
                view.anchor = ViewAnchor {
                    node_id: Some(view.nodes[source.node].node_id.clone()),
                    char_offset: source.offset,
                };
                let first = layout
                    .sources
                    .iter()
                    .position(|r| r.node == source.node && r.offset == source.offset)
                    .unwrap_or(row);
                view.scroll.within = row.saturating_sub(first);
            }
            view.scroll.row = row;
            view.scroll.anchor = view.anchor.clone();
            if amount > 0 && target >= max as isize && end {
                view.follow = true;
                view.unseen = false;
            }
            let boundary = if target < 0 {
                Some(false)
            } else if !end && target >= max as isize {
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
                    if (!forward && node.base_char_offset > 0)
                        || (forward && cached_end < node.total_chars)
                    {
                        view.loading = true;
                        view.scroll.pending = amount;
                        view.scroll.request =
                            Some((view.view_revision, Some(node.node_id.clone())));
                        view.anchor = origin.clone();
                        view.scroll.anchor = origin.clone();
                        view.scroll.within = within;
                        return vec![UiAction::LoadNodeWindow {
                            thread_id,
                            generation: view.generation,
                            view_revision: view.view_revision,
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
                        source.node.checked_sub(1)
                    };
                    if let Some(index) = adjacent {
                        let focus = projection_node_index(view);
                        if (forward && index <= focus) || (!forward && index >= focus) {
                            return vec![];
                        }
                        view.scroll.focus = Some(view.nodes[index].node_id.clone());
                        view.anchor = origin.clone();
                        view.scroll.anchor = origin;
                        view.scroll.within = within;
                        view.scroll.initialized = false;
                        continue;
                    }
                }
                let ordinal = if forward {
                    view.first_ordinal + view.nodes.len() as u64
                } else {
                    view.first_ordinal.saturating_sub(1)
                };
                if (forward && ordinal < view.total_nodes) || (!forward && view.first_ordinal > 0) {
                    // Keep the last visible frame while metadata is in flight.
                    // Its source range is retained when the overlapping page arrives.
                    view.anchor = origin;
                    view.scroll.within = within;
                    view.scroll.row = top;
                    view.loading = true;
                    view.scroll.pending = amount;
                    view.scroll.request = Some((view.view_revision, None));
                    view.scroll.anchor = view.anchor.clone();
                    return vec![UiAction::LoadHistoryWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.view_revision,
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
                    && let Some(thread_id) = view.thread_id.clone()
                {
                    view.loading = true;
                    view.scroll.pending = amount;
                    view.scroll.request = Some((view.view_revision, Some(node.node_id.clone())));
                    return vec![UiAction::LoadNodeWindow {
                        thread_id,
                        generation: view.generation,
                        view_revision: view.view_revision,
                        node_id: node.node_id.clone(),
                        char_offset: view.anchor.char_offset,
                    }];
                }
            }
            return vec![];
        }
        vec![]
    }
    /// Join only proven, contiguous portions of the same selected path. A body-only
    /// reply has no ordinal and therefore must never extend this ordered cache.
    pub(super) fn merge_navigation_window(&mut self, incoming: &mut Vec<UiNode>, first: &mut u64) {
        let old_start = self.first_ordinal;
        let old_end = old_start + self.nodes.len() as u64;
        let new_end = *first + incoming.len() as u64;
        let overlap_start = old_start.max(*first);
        let overlap_end = old_end.min(new_end);
        if overlap_start >= overlap_end
            || (overlap_start..overlap_end).any(|ordinal| {
                self.nodes[(ordinal - old_start) as usize].node_id
                    != incoming[(ordinal - *first) as usize].node_id
            })
        {
            return;
        }
        let mut ordered = incoming
            .drain(..)
            .enumerate()
            .map(|(index, node)| (*first + index as u64, node))
            .collect::<BTreeMap<_, _>>();
        for (index, node) in std::mem::take(&mut self.nodes).into_iter().enumerate() {
            let ordinal = old_start + index as u64;
            if let Some(loaded) = ordered.get_mut(&ordinal) {
                if !node.text.is_empty() {
                    let total = node.total_chars.max(loaded.total_chars);
                    *loaded = node;
                    loaded.total_chars = total;
                }
            } else {
                ordered.insert(ordinal, node);
            }
        }
        *first = old_start.min(*first);
        *incoming = ordered.into_values().collect();
    }
    pub(super) fn viewport_node_index(&self) -> usize {
        if self.follow {
            return self.nodes.len().saturating_sub(1);
        }
        self.anchor
            .node_id
            .as_ref()
            .and_then(|id| self.nodes.iter().position(|node| &node.node_id == id))
            .unwrap_or(0)
    }
    pub(super) fn projection_node_index(&self) -> usize {
        self.scroll
            .focus
            .as_ref()
            .and_then(|id| self.nodes.iter().position(|node| &node.node_id == id))
            .unwrap_or_else(|| self.viewport_node_index())
    }
    pub(super) fn working_node_ids(&self) -> Vec<String> {
        let start = self
            .viewport_node_index()
            .saturating_sub(1)
            .min(self.nodes.len().saturating_sub(3));
        self.nodes[start..(start + 3).min(self.nodes.len())]
            .iter()
            .map(|node| node.node_id.clone())
            .collect()
    }
    pub(super) fn matches(&self, thread: &str, generation: u64) -> bool {
        self.thread_id.as_deref() == Some(thread) && self.generation == generation
    }
}

impl ThreadView {
    pub(super) fn on_thread(&mut self, details: ThreadDetails, draft: Option<DraftSnapshot>) {
        self.draft_restore_pending = false;
        self.cwd = PathBuf::from(details.cwd);
        self.connection = Some(details.connection);
        self.model = Some(details.model);
        self.effort = details.effort;
        self.title = details.title;
        self.run_id = details.run_id;
        self.turn_id = details.turn_id;
        self.state = if details.recovery_reason.is_some() {
            "復旧待ち".into()
        } else if self.run_id.is_some() {
            "処理中".into()
        } else {
            "待機中".into()
        };
        if let Some(reason) = details.recovery_reason {
            let message = serde_json::from_str::<crate::protocol::ApiError>(&reason)
                .map(|error| error.message)
                .unwrap_or_else(|_| "保存された復旧理由を詳細で確認してください".into());
            self.status = format!("復旧待ち: {message}");
        }
        // Only install the saved draft into an untouched placeholder.
        if let Some(draft) = draft.filter(valid_draft)
            && self.draft.revision == 0
            && self.draft.text.is_empty()
            && self.pending.is_empty()
        {
            self.draft = draft;
        }
    }
    pub(super) fn on_history(
        &mut self,
        nodes: Vec<UiNode>,
        before_cursor: Option<String>,
        append: bool,
        first_ordinal: u64,
        selection_revision: u64,
        total_nodes: u64,
    ) -> bool {
        if selection_revision < self.selection_revision {
            return false;
        }
        let mut source_changed = false;
        if self.follow && !self.history_rebase_pending {
            source_changed |= self.retire_committed_preview(&nodes);
        }
        let previous_tail = self.nodes.last().map(|node| node.node_id.clone());
        if self.history_rebase_pending {
            // A tail refresh cannot replace the frame whose source
            // anchor is being rebound to the selected path.
            self.unseen = true;
        } else if append {
            if !nodes.is_empty() {
                source_changed = true;
                self.first_ordinal = first_ordinal;
            }
            let mut previous = nodes;
            previous.retain(|node| {
                !self
                    .nodes
                    .iter()
                    .any(|existing| existing.node_id == node.node_id)
            });
            previous.append(&mut self.nodes);
            self.nodes = previous;
        } else if !self.follow && !self.nodes.is_empty() {
            let incoming_tail = nodes.last().map(|node| node.node_id.as_str());
            if incoming_tail != previous_tail.as_deref() {
                self.unseen = true;
            }
            // A refresh must retain the original viewport, including its
            // window inside a large node. New output is exposed on follow.
            for mut node in nodes {
                if let Some(existing) = self
                    .nodes
                    .iter_mut()
                    .find(|old| old.node_id == node.node_id)
                {
                    if existing.same_projection(&node) {
                        continue;
                    }
                    source_changed = true;
                    if self.anchor.node_id.as_deref() == Some(&node.node_id) {
                        existing.total_chars = node.total_chars;
                        existing.refresh_tools(&node);
                    } else {
                        node.preserve_preview_on_transient_failure(existing);
                        *existing = node;
                    }
                }
            }
        } else if self.first_ordinal == first_ordinal
            && self.selection_revision == selection_revision
            && self
                .nodes
                .iter()
                .filter(|node| !self.provisional_nodes.contains(&node.node_id))
                .count()
                == nodes.len()
            && self
                .nodes
                .iter()
                .filter(|node| !self.provisional_nodes.contains(&node.node_id))
                .zip(&nodes)
                .all(|(old, new)| old.same_projection(new))
        {
            // The final worker projection is unchanged. Preserve owned source
            // and layout; UI compares finite witnesses, never full bodies.
        } else {
            source_changed = true;
            let mut nodes = nodes;
            let preview = self
                .nodes
                .drain(..)
                .filter(|node| {
                    self.provisional_nodes.contains(&node.node_id)
                        && !nodes.iter().any(|saved| saved.node_id == node.node_id)
                })
                .collect::<Vec<_>>();
            nodes.extend(preview);
            self.nodes = nodes;
            self.provisional_nodes
                .retain(|id| self.nodes.iter().any(|node| &node.node_id == id));
            self.first_ordinal = first_ordinal;
            self.selection_revision = selection_revision;
        }
        if source_changed {
            self.mark_source_changed();
        }
        self.observe_sent_nodes();
        self.total_nodes = total_nodes;
        if append || self.follow || previous_tail.is_none() {
            self.before_cursor = before_cursor;
        }
        self.loading = self.scroll.request.is_some();
        if !self.follow
            && !append
            && previous_tail.is_some()
            && previous_tail.as_deref() != self.nodes.last().map(|node| node.node_id.as_str())
        {
            self.unseen = true;
        }

        true
    }
    pub(super) fn on_stream(&mut self, incoming: UiNode) {
        let UiNode {
            node_id,
            role,
            text,
            base_char_offset,
            total_chars,
            reasoning_ranges,
            presentation,
        } = incoming;
        if !text.is_empty() {
            self.activity.stream(&node_id);
        }
        let mut source_changed = false;
        if let Some(node) = self.nodes.iter_mut().find(|n| n.node_id == node_id) {
            let width = |p: &NodePresentation| {
                p.blocks
                    .first()
                    .and_then(|block| block.rendered.as_ref())
                    .map(|rendered| rendered.width)
            };
            if node.total_chars == total_chars
                && node.base_char_offset == base_char_offset
                && node.reasoning_ranges == reasoning_ranges
                && node.presentation.kind == presentation.kind
                && node.presentation.body_extent_unknown == presentation.body_extent_unknown
                && width(&node.presentation) == width(&presentation)
            {
                return;
            }
            source_changed = true;
            if self.follow
                || self.anchor.node_id.as_deref() != Some(&node_id)
                || (self.anchor.char_offset >= base_char_offset
                    && self.anchor.char_offset < base_char_offset + text.chars().count())
            {
                node.text = text;
                node.presentation = presentation;
                node.base_char_offset = base_char_offset;
                node.reasoning_ranges = reasoning_ranges;
            }
            node.total_chars = total_chars;
        } else if self.follow {
            source_changed = true;
            self.provisional_nodes.insert(node_id.clone());
            self.nodes.push(UiNode {
                presentation,
                node_id,
                role,
                text,
                base_char_offset,
                total_chars,
                reasoning_ranges,
            });
        }
        if source_changed {
            self.mark_source_changed();
        }
        if !self.follow {
            self.unseen = true;
        }
    }
    pub(super) fn commit_stream_node(
        &mut self,
        attempt_id: String,
        node_id: String,
        thread_id: String,
        generation: u64,
    ) -> Option<UiAction> {
        if !self.provisional_nodes.contains(&attempt_id) {
            return None;
        }
        if !self.follow && self.anchor.node_id.as_deref() == Some(&attempt_id) {
            self.committed_preview = Some((attempt_id, node_id));
            return self.rebase_preview();
        }
        if !self.follow {
            // Retain the whole paused frame, even when its anchor is on a
            // preceding saved node. Canonical history replaces it atomically.
            return self.rebase_committed_stream(thread_id, generation);
        }
        // A commit identifies the saved source; it does not deliver its body.
        // Keep paid output across separate queue turns until canonical delivery.
        self.committed_nodes.insert(attempt_id, node_id);
        let ready = self
            .committed_nodes
            .values()
            .filter(|saved| {
                self.nodes.iter().any(|node| {
                    &node.node_id == *saved && (node.total_chars == 0 || !node.text.is_empty())
                })
            })
            .cloned()
            .map(|node_id| UiNode {
                node_id,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        if self.retire_committed_preview(&ready) {
            self.mark_source_changed();
        }
        self.rebase_committed_stream(thread_id, generation)
    }
    fn retire_committed_preview(&mut self, incoming: &[UiNode]) -> bool {
        let retired = self
            .committed_nodes
            .iter()
            .filter(|(_, saved)| {
                incoming.iter().any(|node| {
                    &node.node_id == *saved && (node.total_chars == 0 || !node.text.is_empty())
                })
            })
            .map(|(attempt, _)| attempt.clone())
            .collect::<Vec<_>>();
        self.remove_committed_previews(retired)
    }
    fn remove_committed_previews(&mut self, retired: Vec<String>) -> bool {
        for attempt in &retired {
            self.provisional_nodes.remove(attempt);
            self.nodes.retain(|node| &node.node_id != attempt);
            self.committed_nodes.remove(attempt);
            if self
                .committed_preview
                .as_ref()
                .is_some_and(|(id, _)| id == attempt)
            {
                self.committed_preview = None;
            }
        }
        !retired.is_empty()
    }
    fn rebase_preview(&mut self) -> Option<UiAction> {
        if self.history_rebase_pending {
            return None;
        }
        let (attempt, node_id) = self.committed_preview.clone()?;
        let Some(node) = self.nodes.iter().find(|node| node.node_id == attempt) else {
            self.committed_preview = None;
            return None;
        };
        let source = UiNode {
            node_id: node.node_id.clone(),
            role: node.role.clone(),
            text: node.text.clone(),
            base_char_offset: node.base_char_offset,
            total_chars: node.total_chars,
            reasoning_ranges: node.reasoning_ranges.clone(),
            presentation: Default::default(),
        };
        self.cancel_navigation();
        self.history_rebase_pending = true;
        self.loading = true;
        self.scroll.request = Some((self.view_revision, None));
        Some(UiAction::RebaseStream(Box::new(StreamRebase {
            thread_id: self.thread_id.clone()?,
            generation: self.generation,
            view_revision: self.view_revision,
            node_id,
            source,
            anchor: self.anchor.clone(),
        })))
    }
    pub(super) fn rebase_committed_stream(
        &mut self,
        thread_id: String,
        generation: u64,
    ) -> Option<UiAction> {
        if !self.follow
            && !self.history_rebase_pending
            && !self
                .anchor
                .node_id
                .as_ref()
                .is_some_and(|id| self.provisional_nodes.contains(id))
        {
            // Keep this frame while resolving real ordinals and source
            // offsets. No provisional ID is sent to history.get.
            self.cancel_navigation();
            self.history_rebase_pending = true;
            self.loading = true;
            self.scroll.request = Some((self.view_revision, None));
            let action = UiAction::RebaseHistory {
                thread_id,
                generation,
                view_revision: self.view_revision,
                anchor: self.anchor.clone(),
            };
            return Some(action);
        }
        None
    }
    pub(super) fn on_view_state(&mut self, anchor: ViewAnchor, follow: bool) {
        self.anchor = anchor;
        self.follow = follow;
        self.unseen = false;
    }
    pub(super) fn on_load_failed(&mut self, message: String) {
        self.history_rebase_pending = false;
        self.loading = false;
        self.scroll.pending = 0;
        self.scroll.request = None;
        self.status = message;
    }
    pub(super) fn on_node_window(&mut self, view_revision: u64, mut node: UiNode) {
        let node_id_for_reply = node.node_id.clone();
        let body_ready = node.total_chars == 0 || !node.text.is_empty();
        let mut installed = false;
        if let Some(existing) = self
            .nodes
            .iter_mut()
            .find(|existing| existing.node_id == node.node_id)
        {
            installed = true;
            if !existing.same_projection(&node) {
                if !self.follow {
                    node.preserve_preview_on_transient_failure(existing);
                }
                *existing = node;
                self.mark_source_changed();
            }
        }
        // A source-window response does not install an off-page node. Its slot
        // may have been evicted while this asynchronous read was pending.
        if installed && body_ready && self.follow && !self.history_rebase_pending {
            let retired = self
                .committed_nodes
                .iter()
                .filter(|(_, saved)| *saved == &node_id_for_reply)
                .map(|(attempt, _)| attempt.clone())
                .collect::<Vec<_>>();
            if self.remove_committed_previews(retired) {
                self.mark_source_changed();
            }
        }
        if self.scroll.request.is_none()
            || self.scroll.request.as_ref()
                == Some(&(view_revision, Some(node_id_for_reply.clone())))
        {
            self.loading = false;
        }
    }
    pub(super) fn on_history_window(
        &mut self,
        mut nodes: Vec<UiNode>,
        mut first_ordinal: u64,
        selection_revision: u64,
        total_nodes: u64,
        anchor: ViewAnchor,
        window_reply: bool,
    ) {
        if window_reply
            && !self.history_rebase_pending
            && selection_revision == self.selection_revision
        {
            merge_navigation_window(self, &mut nodes, &mut first_ordinal);
        }
        let relocated = self.history_rebase_pending
            && !nodes
                .iter()
                .any(|node| Some(&node.node_id) == self.anchor.node_id.as_ref());
        self.mark_source_changed();
        self.nodes = nodes;
        self.observe_sent_nodes();
        self.provisional_nodes.clear();
        self.committed_nodes.clear();
        self.committed_preview = None;
        self.first_ordinal = first_ordinal;
        self.selection_revision = selection_revision;
        self.total_nodes = total_nodes;
        if !window_reply
            || (self.history_rebase_pending && self.anchor != anchor)
            || !self
                .nodes
                .iter()
                .any(|node| Some(&node.node_id) == self.anchor.node_id.as_ref())
        {
            self.anchor = anchor;
            self.scroll.within = 0;
        }
        self.follow = false;
        self.before_cursor = None;
        self.loading = false;
        if self.history_rebase_pending {
            self.history_rebase_pending = false;
            self.status = if relocated {
                "表示元が選択経路から外れたため、最新ページを表示しました"
            } else {
                "履歴の表示位置を再取得しました"
            }
            .into();
        }
    }
    pub(super) fn on_history_invalidated(
        &mut self,
        thread_id: String,
        generation: u64,
        view_revision: Option<u64>,
    ) -> Option<UiAction> {
        if self.thread_id.as_deref() != Some(&thread_id)
            || self.generation != generation
            || view_revision.is_some_and(|revision| revision != self.view_revision)
            || (view_revision.is_none() && self.follow)
            || self.history_rebase_pending
        {
            return None;
        }
        self.cancel_navigation();
        self.history_rebase_pending = true;
        self.loading = true;
        self.scroll.request = Some((self.view_revision, None));
        self.status = "履歴の表示位置を再取得中…".into();
        let action = UiAction::RebaseHistory {
            thread_id,
            generation,
            view_revision: self.view_revision,
            anchor: self.anchor.clone(),
        };
        Some(action)
    }
    pub(super) fn on_tool_facts(
        &mut self,
        result: Result<Vec<UiNode>, String>,
    ) -> Option<UiAction> {
        self.tool_refresh = None;
        if let Ok(nodes) = result {
            let mut changed = false;
            for node in nodes {
                if let Some(existing) = self
                    .nodes
                    .iter_mut()
                    .find(|existing| existing.node_id == node.node_id)
                {
                    changed |= existing.refresh_tools(&node);
                }
            }
            if changed {
                self.mark_source_changed();
            }
        } else {
            for node in &mut self.nodes {
                for block in &mut node.presentation.blocks {
                    if let Some(tool) = &mut block.tool {
                        tool.observation_unavailable();
                    }
                }
            }
            self.mark_source_changed();
        }

        self.request_tool_refresh()
    }
}

impl ThreadView {
    pub(super) fn on_receipt(
        &mut self,
        draft_id: String,
        revision: u64,
        thread_id: Option<String>,
        status: ReceiptStatus,
        message: Option<String>,
    ) -> bool {
        let Some(pending) = self.pending.get_mut(&revision) else {
            return false;
        };
        // Status cannot regress after an accepted or terminal receipt.
        if matches!(
            pending.status,
            ReceiptStatus::Failed | ReceiptStatus::Cancelled | ReceiptStatus::Completed
        ) && status != pending.status
        {
            return false;
        }
        if (status == ReceiptStatus::Accepting && pending.status != ReceiptStatus::Accepting)
            || (status == ReceiptStatus::Accepted
                && matches!(
                    pending.status,
                    ReceiptStatus::Preparing | ReceiptStatus::Held
                ))
        {
            return false;
        }
        pending.status = status.clone();
        pending.message = message.clone();
        if let Some(thread_id) = thread_id {
            self.thread_id = Some(thread_id);
        }
        let accepted = matches!(
            status,
            ReceiptStatus::Accepted
                | ReceiptStatus::Preparing
                | ReceiptStatus::Held
                | ReceiptStatus::Completed
        );
        if accepted && self.draft.draft_id == draft_id && self.draft.revision == revision {
            self.draft.text = String::new();
            self.draft.attachments = Vec::new();
            self.draft.folds = Vec::new();
            self.draft.cursor = 0;
            self.draft.literal_paste = false;
            self.draft.edit = None;
            self.draft.revision += 1;
        }
        self.status = format!(
            "{}{}",
            if matches!(status, ReceiptStatus::Failed | ReceiptStatus::Cancelled) {
                format!(
                    "{} · /restore {revision} で元の入力を復元",
                    receipt_label(&status)
                )
            } else {
                receipt_label(&status).into()
            },
            message.map(|m| format!(": {m}")).unwrap_or_default()
        );
        if accepted
            && self.draft.draft_id == draft_id
            && self.draft.revision != revision
            && (!self.draft.text.is_empty() || !self.draft.attachments.is_empty())
        {
            self.status
                .push_str(" · 編集中の入力を保持（次は全文送信）");
        }
        if status == ReceiptStatus::Completed
            && (pending.send.is_none() || pending.source_node_id.is_some())
        {
            self.pending.remove(&revision);
        }
        true
    }
    pub(super) fn on_restored_snapshot(
        &mut self,
        revision: u64,
        expected_revision: u64,
        text: String,
        attachments: Vec<UiAttachment>,
    ) {
        let pending = self
            .pending
            .get(&revision)
            .expect("caller checked source snapshot");
        let mut draft = pending.snapshot.clone();
        draft.revision = expected_revision + 1;
        draft.cursor = draft.cursor.min(text.len());
        draft.cursor = text.floor_char_boundary(draft.cursor);
        draft.text = text;
        draft.attachments = attachments;
        draft.literal_paste = true;
        self.draft = draft;
        self.status = "保存済みの依頼を入力欄へ戻しました".into();
    }
    pub(super) fn on_clipboard_attachment(
        &mut self,
        attachment: UiAttachment,
        limit: usize,
    ) -> bool {
        let position = attachment.position.unwrap_or(self.draft.cursor);
        if !self.draft.text.is_char_boundary(position)
            || self.draft.text.len().saturating_add(draft::MARKER.len()) > limit
        {
            self.status = "画像位置または下書きのbyte上限が変わったため採用していません".into();
            false
        } else {
            self.draft.insert_attachment(attachment, position);
            self.draft.revision += 1;
            self.status = "画像を添付しました".into();
            true
        }
    }
    pub(super) fn on_progress(&mut self, tool_calls: u64, cancelled_calls: u64, state: String) {
        self.tool_calls = tool_calls;
        self.cancelled_calls = cancelled_calls;
        self.state = state;
    }
}

impl ThreadView {
    pub(super) fn retain_source_ids(&mut self) {
        self.provisional_nodes
            .retain(|id| self.nodes.iter().any(|node| &node.node_id == id));
        self.committed_nodes
            .retain(|attempt, _| self.provisional_nodes.contains(attempt));
        if self
            .committed_preview
            .as_ref()
            .is_some_and(|(id, _)| !self.provisional_nodes.contains(id))
        {
            self.committed_preview = None;
        }
        self.reasoning_expanded
            .retain(|id| self.nodes.iter().any(|node| &node.node_id == id));
    }
    pub(super) fn compact_node_slots(&mut self) -> usize {
        let old = self.nodes.capacity();
        if old > self.nodes.len() {
            self.nodes = std::mem::take(&mut self.nodes)
                .into_boxed_slice()
                .into_vec();
        }
        (old - self.nodes.capacity()) * std::mem::size_of::<UiNode>()
    }
    pub(super) fn discard_cached_history(&mut self) {
        self.nodes = Vec::new();
        self.committed_nodes.clear();
        self.provisional_nodes.clear();
        self.committed_preview = None;
        self.reasoning_expanded = BTreeSet::new();
        self.mark_source_changed();
    }
    pub(super) fn trim_page_edges(
        &mut self,
        working: &[String],
        retained: &mut usize,
        budget: usize,
    ) {
        // Remove only page edges outside the fixed working set, keeping path
        // ordinals contiguous. The counter anticipates compaction below.
        while *retained > budget && self.nodes.len() > working.len() {
            let last = self.nodes.len() - 1;
            let index = if working.contains(&self.nodes[0].node_id) {
                last
            } else if working.contains(&self.nodes[last].node_id)
                || self.viewport_node_index() > self.nodes.len() / 2
            {
                0
            } else {
                last
            };
            let removed = self.nodes.remove(index);
            if let Some((attempt, saved)) = self.committed_nodes.remove_entry(&removed.node_id) {
                *retained = retained
                    .saturating_sub(expanded_id_bytes(&attempt) + expanded_id_bytes(&saved));
            }
            *retained = retained.saturating_sub(removed.metadata_bytes() + removed.cached_bytes());
            if let Some(id) = self.reasoning_expanded.take(&removed.node_id) {
                *retained = retained.saturating_sub(expanded_id_bytes(&id));
            }
            if index == 0 {
                self.first_ordinal += 1;
            }
            self.mark_source_changed();
        }
        if self.nodes.capacity() > self.nodes.len() {
            self.nodes = std::mem::take(&mut self.nodes)
                .into_boxed_slice()
                .into_vec();
        }
    }
    pub(super) fn retained_body_bytes(&self, working: &[String]) -> usize {
        let mut bytes =
            self.nodes.capacity().saturating_sub(working.len()) * std::mem::size_of::<UiNode>();
        for node in &self.nodes {
            if !working.contains(&node.node_id) {
                bytes = bytes.saturating_add(
                    node.metadata_bytes() - std::mem::size_of::<UiNode>() + node.cached_bytes(),
                );
            }
        }
        for (attempt, saved) in &self.committed_nodes {
            if !working.contains(attempt) {
                bytes = bytes.saturating_add(expanded_id_bytes(attempt) + expanded_id_bytes(saved));
            }
        }
        for id in &self.reasoning_expanded {
            if !working.contains(id) {
                bytes = bytes.saturating_add(expanded_id_bytes(id));
            }
        }
        bytes
    }
    pub(super) fn evict_node_body(&mut self, index: usize) -> usize {
        let node = &mut self.nodes[index];
        let mut bytes = node.cached_bytes();
        let source_changed = bytes != 0;
        if source_changed {
            node.evict_body();
        }
        if let Some(id) = self.reasoning_expanded.take(&node.node_id) {
            bytes = bytes.saturating_add(expanded_id_bytes(&id));
        }
        if source_changed {
            self.mark_source_changed();
        }
        bytes
    }
}

impl ThreadView {
    /// Retained source nodes are read-only outside their owning view.
    pub fn nodes(&self) -> &Vec<UiNode> {
        &self.nodes
    }
    #[cfg(test)]
    pub(super) fn nodes_mut_for_test(&mut self) -> &mut Vec<UiNode> {
        self.mark_source_changed();
        &mut self.nodes
    }
}

impl ThreadView {
    pub(super) fn matches_navigation_reply(
        &self,
        thread: &str,
        generation: u64,
        revision: u64,
        node: Option<&str>,
    ) -> bool {
        self.matches(thread, generation)
            && self.view_revision == revision
            && self
                .scroll
                .request
                .as_ref()
                .is_some_and(|(expected, id)| *expected == revision && id.as_deref() == node)
    }
    pub(super) fn finish_navigation_reply(&mut self) -> isize {
        self.scroll.initialized = false;
        self.scroll.request = None;
        std::mem::take(&mut self.scroll.pending)
    }
}
impl ThreadView {
    pub(super) fn history_revision(&self) -> u64 {
        self.history_revision
    }
    fn mark_source_changed(&mut self) {
        self.history_revision = self.history_revision.wrapping_add(1);
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    #[test]
    fn scroll_uses_its_own_sources_and_preserves_source_anchor_across_widths() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        let text = (0..60)
            .map(|n| format!("source line {n:02} 日本語 words for wrapping\n"))
            .collect::<String>();
        view.on_history(
            vec![UiNode {
                node_id: "source".into(),
                role: "assistant".into(),
                total_chars: text.chars().count(),
                text,
                base_char_offset: 0,
                reasoning_ranges: vec![],
                presentation: Default::default(),
            }],
            None,
            false,
            0,
            1,
            1,
        );
        view.follow = false;
        view.anchor = ViewAnchor {
            node_id: Some("source".into()),
            char_offset: 0,
        };
        let mut cache = HistoryLayoutCache::default();
        let budget = 1024 * 1024;
        let layout = view.history_layout("owner", &mut cache, budget, 35, 8);
        let mut previous = view.viewport_row(&layout, 35, 8);
        for _ in 0..30 {
            assert!(view.scroll(1, "owner", &mut cache, budget).is_empty());
            let layout = view.history_layout("owner", &mut cache, budget, 35, 8);
            let row = view.viewport_row(&layout, 35, 8);
            assert_eq!(
                row,
                previous + 1,
                "cached single-row scroll never jumps backwards"
            );
            assert_eq!(layout.sources[row].offset, view.anchor.char_offset);
            previous = row;
        }
        let anchor = view.anchor.clone();
        let resized = view.history_layout("owner", &mut cache, budget, 55, 8);
        let row = view.viewport_row(&resized, 55, 8);
        assert_eq!(view.anchor, anchor);
        assert!(resized.sources[row].offset <= anchor.char_offset);
        assert!(cache.bytes <= budget);
    }
}

impl ThreadView {
    pub(super) fn settle_stream(&mut self, attempt: &str) -> Option<UiAction> {
        // A stale terminal read must not discard a source already committed.
        if self.committed_nodes.contains_key(attempt) {
            return None;
        }
        if !self.provisional_nodes.remove(attempt) {
            return None;
        }
        if self
            .committed_preview
            .as_ref()
            .is_some_and(|(id, _)| id == attempt)
        {
            self.committed_preview = None;
        }
        self.nodes.retain(|node| node.node_id != attempt);
        self.mark_source_changed();
        if self.anchor.node_id.as_deref() != Some(attempt) {
            return None;
        }
        // A transient stream position is not a canonical source offset.
        self.anchor = ViewAnchor::default();
        if self.follow {
            self.scroll.initialized = false;
            return None;
        }
        self.cancel_navigation();
        self.history_rebase_pending = true;
        self.loading = true;
        self.scroll.request = Some((self.view_revision, None));
        Some(UiAction::RebaseHistory {
            thread_id: self.thread_id.clone()?,
            generation: self.generation,
            view_revision: self.view_revision,
            anchor: self.anchor.clone(),
        })
    }
}

impl ThreadView {
    pub(super) fn provisional_sources(&self) -> Vec<UiNode> {
        self.nodes
            .iter()
            .rev()
            .filter(|node| {
                self.provisional_nodes.contains(&node.node_id) && node.role != "received_response"
            })
            .take(2)
            .map(|node| UiNode {
                node_id: node.node_id.clone(),
                role: node.role.clone(),
                text: node.text.clone(),
                base_char_offset: node.base_char_offset,
                total_chars: node.total_chars,
                reasoning_ranges: node.reasoning_ranges.clone(),
                presentation: NodePresentation {
                    provisional_stream: node.presentation.provisional_stream,
                    body_extent_unknown: node.presentation.body_extent_unknown,
                    ..Default::default()
                },
            })
            .collect()
    }
    pub(super) fn settle_other_streams(&mut self, current: &str) -> Vec<UiAction> {
        let previous = self
            .provisional_nodes
            .iter()
            .filter(|id| {
                id.as_str() != current
                    && !self
                        .nodes
                        .iter()
                        .any(|n| &n.node_id == *id && n.role == "received_response")
            })
            .cloned()
            .collect::<Vec<_>>();
        previous
            .into_iter()
            .filter_map(|id| self.settle_stream(&id))
            .collect()
    }
}

#[cfg(test)]
mod stream_refresh_tests {
    use super::*;
    fn stream(text: &str) -> UiNode {
        UiNode {
            node_id: "attempt".into(),
            role: "assistant".into(),
            text: text.into(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: Default::default(),
        }
    }
    #[test]
    fn metadata_history_does_not_erase_preview_or_reinterpret_it_as_canonical() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.thread_id = Some("thread".into());
        view.on_stream(stream("前半"));
        assert_eq!(view.provisional_sources()[0].node_id, "attempt");
        view.on_history(vec![], None, false, 0, 0, 0);
        assert_eq!(view.nodes()[0].text, "前半");
        view.on_stream(stream("前半後半"));
        assert_eq!(view.nodes()[0].text, "前半後半");
        let revision = view.history_revision();
        view.on_stream(stream("前半後半"));
        assert_eq!(view.history_revision(), revision);
        view.follow = false;
        view.anchor = ViewAnchor {
            node_id: Some("attempt".into()),
            char_offset: 2,
        };
        let action = view.settle_stream("attempt").unwrap();
        assert!(view.nodes().is_empty());
        assert!(view.anchor.node_id.is_none());
        assert!(matches!(
            action,
            UiAction::RebaseHistory {
                anchor: ViewAnchor {
                    node_id: None,
                    char_offset: 0
                },
                ..
            }
        ));
    }
}

#[cfg(test)]
mod identical_projection_tests {
    use super::*;
    fn prepared(text: &str) -> UiNode {
        let mut node = UiNode {
            node_id: "saved".into(),
            role: "assistant".into(),
            text: text.into(),
            total_chars: text.chars().count(),
            ..Default::default()
        };
        crate::frontend::stream_metrics::seal(&mut node);
        node
    }
    #[test]
    fn identical_final_history_retains_layout_and_different_source_envelope_or_width_invalidates() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        let node = prepared(&"unchanged body\n".repeat(200));
        view.on_history(vec![node.clone()], None, false, 0, 1, 1);
        let mut cache = HistoryLayoutCache::default();
        let budget = 1024 * 1024;
        let first = view.history_layout("owner", &mut cache, budget, 80, 20);
        let revision = view.history_revision();
        for _ in 0..10 {
            view.on_history(vec![node.clone()], None, false, 0, 1, 1);
            let next = view.history_layout("owner", &mut cache, budget, 80, 20);
            assert!(Arc::ptr_eq(&first, &next));
        }
        assert_eq!(view.history_revision(), revision);
        assert_eq!(cache.builds, 1);
        for mut changed in [
            prepared("changed body"),
            node.clone(),
            node.clone(),
            node.clone(),
        ] {
            // Independently alter envelope metadata, source range or rendered
            // width after final worker preparation, then reseal.
            if changed.text == node.text {
                match cache.builds {
                    2 => changed.presentation.title = "changed metadata".into(),
                    3 => {
                        changed.base_char_offset = 1;
                        changed.total_chars += 1;
                    }
                    _ => changed
                        .presentation
                        .blocks
                        .push(crate::presentation::PresentationBlock {
                            rendered: Some(crate::presentation::RenderedMarkdown {
                                width: 35,
                                lines: vec![],
                                omitted: false,
                            }),
                            ..Default::default()
                        }),
                }
                crate::frontend::stream_metrics::seal(&mut changed);
            }
            view.on_history(vec![changed], None, false, 0, 1, 1);
            view.history_layout("owner", &mut cache, budget, 80, 20);
        }
        assert_eq!(cache.builds, 5);
    }
}

#[cfg(test)]
mod completion_notice_tests {
    use super::*;
    fn notice(generation: u64, revision: u64, node: &str, message: &str) -> UiUpdate {
        UiUpdate::HistoryRebaseNotice {
            thread_id: "thread".into(),
            generation,
            view_revision: revision,
            anchor: ViewAnchor {
                node_id: Some(node.into()),
                char_offset: 0,
            },
            message: message.into(),
        }
    }
    #[test]
    fn completion_notice_requires_the_accepted_owner_revision_anchor_and_paused_view() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        let revision = ui.view().view_revision;
        ui.view_mut().follow = false;
        // Model the accepted HistoryWindow, not the initial selection load.
        ui.view_mut().loading = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 0,
        };
        ui.view_mut().status = "reader moved to latest path".into();
        ui.apply(notice(generation, revision, "off-path", "wrong"));
        ui.apply(notice(generation + 1, revision, "saved", "wrong"));
        ui.apply(notice(generation, revision + 1, "saved", "wrong"));
        assert_eq!(ui.view().status, "reader moved to latest path");
        ui.apply(notice(generation, revision, "saved", "verified source"));
        assert_eq!(ui.view().status, "verified source");
        ui.view_mut().follow = true;
        ui.apply(notice(generation, revision, "saved", "stale after follow"));
        assert_eq!(ui.view().status, "verified source");
    }
    #[test]
    fn rejected_stream_rebase_can_retry_and_follow_completion_releases_its_correspondence() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        ui.apply(UiUpdate::Stream {
            thread_id: "thread".into(),
            generation,
            node_id: "attempt".into(),
            role: "assistant".into(),
            text: "bounded source".into(),
            base_char_offset: 0,
            total_chars: 14,
            reasoning_ranges: vec![],
            presentation: Default::default(),
            ack: None,
        });
        ui.view_mut().follow = false;
        ui.view_mut().anchor = ViewAnchor {
            node_id: Some("attempt".into()),
            char_offset: 7,
        };
        let commit = || UiUpdate::StreamCommitted {
            thread_id: "thread".into(),
            generation,
            attempt_id: "attempt".into(),
            node_id: "saved".into(),
        };
        ui.apply(commit());
        let action = ui.take_deferred_actions().pop().unwrap();
        ui.action_rejected(&action);
        assert!(!ui.view().loading);
        assert!(ui.view().committed_preview.is_some());
        let key = ui.active.clone();
        let retry =
            ui.views
                .get_mut(&key)
                .unwrap()
                .scroll(1, &key, &mut ui.layout_cache, 1024 * 1024);
        assert!(
            matches!(retry.as_slice(),[UiAction::RebaseStream(request)]if request.source.text=="bounded source"&&request.anchor.char_offset==7)
        );
        let old_revision = ui.view().view_revision;
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::End,
            KeyModifiers::CONTROL,
        )));
        assert!(ui.view().follow);
        assert_ne!(ui.view().view_revision, old_revision);
        ui.apply(commit());
        assert!(!ui.view().committed_nodes.is_empty());
        assert_eq!(ui.view().nodes()[0].text, "bounded source");
        // A late accepted window from before Ctrl+End cannot pull follow back.
        ui.apply(UiUpdate::HistoryWindow {
            thread_id: "thread".into(),
            generation,
            view_revision: old_revision,
            nodes: vec![],
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 0,
            anchor: ViewAnchor {
                node_id: Some("saved".into()),
                char_offset: 7,
            },
        });
        assert!(ui.view().follow);
    }
}

impl ThreadView {
    pub(super) fn retain_incomplete_stream(&mut self, attempt: &str, source: String, kind: &str) {
        self.committed_nodes.remove(attempt);
        if self.nodes.iter().any(|n| n.node_id == source) {
            self.provisional_nodes.remove(attempt);
            self.nodes.retain(|n| n.node_id != attempt);
            if self.anchor.node_id.as_deref() == Some(attempt) {
                self.anchor.node_id = Some(source);
                self.anchor.char_offset = 0;
            }
            self.mark_source_changed();
            return;
        }
        if let Some(node) = self.nodes.iter_mut().find(|n| n.node_id == attempt) {
            self.provisional_nodes.remove(attempt);
            self.provisional_nodes.insert(source.clone());
            if self.anchor.node_id.as_deref() == Some(attempt) {
                self.anchor.node_id = Some(source.clone());
            }
            node.node_id = source;
            node.role = "received_response".into();
            node.presentation.kind = crate::presentation::PresentationKind::Interrupted;
            node.presentation.title = if kind == "model_stream_observation" {
                "ストリーム差分 · 最終応答は確定済み"
            } else {
                "未確定の受信本文 · モデル再送対象外"
            }
            .into();
            node.presentation.provisional_stream = false;
            for block in &mut node.presentation.blocks {
                block.source_exact = false;
            }
            node.presentation.projection_stamp = None;
            self.mark_source_changed();
        }
    }
    pub(super) fn on_incomplete_responses(&mut self, nodes: Vec<UiNode>) {
        let ids = nodes
            .iter()
            .map(|n| n.node_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let previous = self.nodes.len();
        self.nodes
            .retain(|n| n.role != "received_response" || ids.contains(&n.node_id));
        let mut changed = previous != self.nodes.len();
        self.provisional_nodes
            .retain(|id| self.nodes.iter().any(|n| &n.node_id == id));
        for incoming in nodes {
            self.provisional_nodes.insert(incoming.node_id.clone());
            if let Some(existing) = self
                .nodes
                .iter_mut()
                .find(|n| n.node_id == incoming.node_id)
            {
                if !existing.same_projection(&incoming) {
                    if !self.follow && self.anchor.node_id.as_deref() == Some(&incoming.node_id) {
                        existing.total_chars = incoming.total_chars;
                    } else {
                        *existing = incoming;
                    }
                    changed = true;
                }
            } else {
                self.nodes.push(incoming);
                changed = true;
            }
        }
        if changed {
            self.mark_source_changed();
        }
    }
}

#[cfg(test)]
mod received_tests {
    use super::*;
    #[test]
    fn terminal_failure_keeps_visible_paid_text_and_fresh_generations_own_saved_previews() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let generation = ui.view().generation;
        ui.apply(UiUpdate::Stream {
            thread_id: "t".into(),
            generation,
            node_id: "a".into(),
            role: "assistant".into(),
            text: "受信本文".into(),
            base_char_offset: 0,
            total_chars: 4,
            reasoning_ranges: vec![],
            presentation: crate::presentation::project_stream_window("受信本文", 0, &[], 80),
            ack: None,
        });
        ui.apply(UiUpdate::StreamSettled {
            thread_id: "t".into(),
            generation,
            attempt_id: "a".into(),
            received_source: Some(("audit".into(), "incomplete_response".into())),
            ack: None,
        });
        assert_eq!(ui.view().nodes()[0].text, "受信本文");
        assert_eq!(ui.view().nodes()[0].node_id, "audit");
        assert!(ui.view().nodes()[0].presentation.title.contains("未確定"));
        ui.apply(UiUpdate::Stream {
            thread_id: "t".into(),
            generation,
            node_id: "next".into(),
            role: "assistant".into(),
            text: "次".into(),
            base_char_offset: 0,
            total_chars: 1,
            reasoning_ranges: vec![],
            presentation: Default::default(),
            ack: Some(Arc::new(tokio::sync::Notify::new())),
        });
        assert!(ui.view().nodes().iter().any(|n| n.node_id == "audit"));
        ui.select_thread("other".into());
        ui.apply(UiUpdate::IncompleteResponses {
            thread_id: "t".into(),
            generation: generation + 100,
            nodes: vec![],
        });
        assert_eq!(ui.view().thread_id.as_deref(), Some("other"));
    }
}

#[cfg(test)]
mod committed_correspondence_tests {
    use super::*;

    #[test]
    fn two_commits_resolve_reverse_body_delivery_without_losing_the_other_source() {
        fn source(id: &str, text: &str, total_chars: usize) -> UiNode {
            UiNode {
                node_id: id.into(),
                role: "assistant".into(),
                text: text.into(),
                total_chars,
                ..Default::default()
            }
        }
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.thread_id = Some("thread".into());
        for (attempt, saved, paid) in [
            ("attempt-a", "saved-a", "paid A"),
            ("attempt-b", "saved-b", "paid B"),
        ] {
            view.on_stream(source(attempt, paid, paid.chars().count()));
            view.commit_stream_node(attempt.into(), saved.into(), "thread".into(), 0);
        }
        assert_eq!(view.committed_nodes.len(), 2);
        view.on_history(
            vec![source("saved-a", "", 7), source("saved-b", "", 7)],
            None,
            false,
            0,
            1,
            2,
        );
        assert_eq!(view.committed_nodes.len(), 2);
        assert!(view.nodes().iter().any(|node| node.text == "paid A"));
        assert!(view.nodes().iter().any(|node| node.text == "paid B"));

        let revision = view.view_revision;
        view.on_node_window(revision, source("saved-b", "final B", 7));
        assert_eq!(view.committed_nodes.len(), 1);
        assert!(view.committed_nodes.contains_key("attempt-a"));
        assert!(view.nodes().iter().any(|node| node.text == "paid A"));
        assert!(!view.nodes().iter().any(|node| node.node_id == "attempt-b"));
        assert_eq!(
            view.nodes()
                .iter()
                .filter(|node| node.text == "final B")
                .count(),
            1
        );
        view.on_node_window(revision, source("saved-b", "final B", 7));
        assert_eq!(
            view.nodes()
                .iter()
                .filter(|node| node.text == "final B")
                .count(),
            1
        );
        assert!(view.nodes().iter().any(|node| node.text == "paid A"));

        view.on_node_window(revision, source("saved-a", "final A", 7));
        assert!(view.committed_nodes.is_empty());
        assert!(view.provisional_nodes.is_empty());
        assert_eq!(view.nodes().len(), 2);
        assert_eq!(
            view.nodes()
                .iter()
                .filter(|node| node.text == "final A")
                .count(),
            1
        );
        assert_eq!(
            view.nodes()
                .iter()
                .filter(|node| node.text == "final B")
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod evicted_committed_source_tests {
    use super::*;

    #[test]
    fn an_evicted_canonical_slot_cannot_retire_the_paid_preview_from_a_late_window() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.thread_id = Some("thread".into());
        view.on_stream(UiNode {
            node_id: "attempt".into(),
            role: "assistant".into(),
            text: "paid body".into(),
            total_chars: 9,
            ..Default::default()
        });
        view.commit_stream_node("attempt".into(), "saved".into(), "thread".into(), 0);
        view.on_history(
            vec![UiNode {
                node_id: "saved".into(),
                total_chars: 10,
                ..Default::default()
            }],
            None,
            false,
            0,
            1,
            1,
        );
        let pending_revision = view.view_revision;
        // Same effect as a page refresh/edge eviction while the window read
        // waits: generation/view_revision stay valid, but its slot is gone.
        view.nodes.retain(|node| node.node_id != "saved");
        view.on_node_window(
            pending_revision,
            UiNode {
                node_id: "saved".into(),
                role: "assistant".into(),
                text: "final body".into(),
                total_chars: 10,
                ..Default::default()
            },
        );
        assert_eq!(view.nodes().len(), 1);
        assert_eq!(view.nodes()[0].text, "paid body");
        assert!(view.committed_nodes.contains_key("attempt"));
        assert!(view.provisional_nodes.contains("attempt"));
        view.on_history(
            vec![UiNode {
                node_id: "saved".into(),
                role: "assistant".into(),
                text: "final body".into(),
                total_chars: 10,
                ..Default::default()
            }],
            None,
            false,
            0,
            2,
            1,
        );
        assert_eq!(view.nodes().len(), 1);
        assert_eq!(view.nodes()[0].node_id, "saved");
        assert_eq!(view.nodes()[0].text, "final body");
        assert!(view.committed_nodes.is_empty());
        assert!(view.provisional_nodes.is_empty());
    }
}
