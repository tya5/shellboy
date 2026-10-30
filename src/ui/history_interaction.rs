//! Ephemeral conversation focus and bounded explicit tool previews. No I/O.
use super::*;
use history_items::{ItemRegion, Preview};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoticeKind {
    Info,
    Failure,
}
#[derive(Clone, Debug)]
pub(super) struct OperationNotice {
    pub text: String,
    pub kind: NoticeKind,
}

pub(super) fn key_bytes(key: &ItemKey) -> usize {
    match key {
        ItemKey::Node(id) => id.capacity(),
        ItemKey::Tool { group_id, call_id } => group_id.capacity() + call_id.capacity(),
        ItemKey::LocalTool { node_id, .. } => node_id.capacity(),
    }
}
pub(super) fn region_bytes(sources: &[presentation::RowSource]) -> usize {
    let mut seen = BTreeSet::new();
    sources
        .iter()
        .filter_map(|row| row.item.as_ref())
        .filter(|item| seen.insert(Arc::as_ptr(item) as usize))
        .map(|item| {
            std::mem::size_of::<ItemRegion>()
                + 32
                + key_bytes(&item.key)
                + item.primary_node_id.capacity()
                + item.execution_id.as_ref().map_or(0, String::capacity)
                + item.source_node_ids.capacity() * std::mem::size_of::<String>()
                + item
                    .source_node_ids
                    .iter()
                    .map(String::capacity)
                    .sum::<usize>()
                + item.outcome.capacity()
        })
        .sum()
}

impl ThreadView {
    pub(super) fn clear_operation_notice(&mut self) {
        if self.operation_notice.take().is_some() {
            self.status = self.state.clone();
        }
    }
    pub(super) fn promote_status_notice(&mut self, kind: NoticeKind) {
        if self.status.is_empty()
            || self.status == self.state
            || self.status == self.footer_phase().label()
            || matches!(
                self.status.as_str(),
                "入力できます" | "待機中" | "処理中" | "完了" | "操作を準備しました"
            )
            || self
                .operation_notice
                .as_ref()
                .is_some_and(|notice| notice.text == self.status)
        {
            return;
        }
        // An unrelated background status cannot erase a failed operation.
        if kind == NoticeKind::Info
            && self
                .operation_notice
                .as_ref()
                .is_some_and(|notice| notice.kind == NoticeKind::Failure)
        {
            return;
        }
        // Saved recovery has its own footer; independent operation failures
        // must remain visible even while that recovery still exists.
        if self
            .activity
            .snapshot
            .as_ref()
            .and_then(|s| s.recovery.as_ref())
            .is_some_and(|recovery| recovery.message == self.status)
        {
            return;
        }
        self.operation_notice = Some(OperationNotice {
            text: self.status.clone(),
            kind,
        });
    }
}

impl UiState {
    fn item_layout(&mut self) -> Arc<HistoryLayout> {
        let width = self.view().scroll.width;
        let height = self.view().scroll.height;
        self.history_layout(width, height)
    }
    pub(super) fn selected_history_item(&mut self) -> Option<Arc<ItemRegion>> {
        let layout = self.item_layout();
        if self.view().items.active {
            if let Some(key) = &self.view().items.focus
                && let Some(item) = layout
                    .sources
                    .iter()
                    .filter_map(|s| s.item.as_ref())
                    .find(|item| &item.key == key)
            {
                let item = Arc::clone(item);
                self.active_view_mut().history_focus_item = Some(Arc::clone(&item));
                return Some(item);
            }
            if let Some(item) = &self.view().history_focus_item {
                return Some(Arc::clone(item));
            }
        }
        let row = self.viewport_row(&layout, self.view().scroll.width, self.view().scroll.height);
        layout
            .sources
            .iter()
            .skip(row)
            .take(usize::from(self.view().scroll.height))
            .rev()
            .find_map(|source| source.item.as_ref().map(Arc::clone))
    }
    fn visible_item_choices(&mut self) -> (Arc<HistoryLayout>, Vec<(usize, Arc<ItemRegion>)>) {
        let layout = self.item_layout();
        let mut seen = BTreeSet::new();
        let items = layout
            .sources
            .iter()
            .enumerate()
            .filter_map(|(row, source)| {
                let item = source.item.as_ref()?;
                seen.insert(item.key.clone())
                    .then(|| (row, Arc::clone(item)))
            })
            .collect();
        (layout, items)
    }
    fn select_history_item(&mut self, row: usize, item: Arc<ItemRegion>, layout: &HistoryLayout) {
        let width = self.view().scroll.width;
        let height = self.view().scroll.height;
        let current = self.viewport_row(layout, width, height);
        let height = usize::from(height).max(1);
        let top = if row < current {
            row
        } else if row >= current.saturating_add(height) {
            row + 1 - height
        } else {
            current
        };
        let source = layout.sources.get(top);
        let view = self.active_view_mut();
        view.cancel_navigation();
        view.items.active = true;
        view.items.focus = Some(item.key.clone());
        view.history_focus_item = Some(item);
        view.follow = false;
        if let Some(source) = source
            && let Some(node) = view.nodes().get(source.node)
        {
            view.anchor = ViewAnchor {
                node_id: Some(node.node_id.clone()),
                char_offset: source.offset,
            };
            view.scroll.anchor = view.anchor.clone();
            let first = layout
                .sources
                .iter()
                .position(|candidate| {
                    candidate.node == source.node && candidate.offset == source.offset
                })
                .unwrap_or(top);
            view.scroll.within = top.saturating_sub(first);
            view.scroll.row = top;
            view.scroll.revision = view.history_revision();
            view.scroll.origin = layout.sources.first().map_or(0, |source| source.node);
            view.scroll.initialized = true;
        }
        self.dirty = true;
    }
    pub(super) fn history_focus_key(&mut self, key: KeyEvent) -> Option<Vec<UiAction>> {
        if !self.composer_active() {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::End {
                self.active_view_mut().items.active = false;
            }
            return None;
        }
        if key.code == KeyCode::Tab && !self.view().items.active {
            if self.has_completions() {
                return None;
            }
            let layout = self.item_layout();
            let row =
                self.viewport_row(&layout, self.view().scroll.width, self.view().scroll.height);
            if let Some((index, item)) = layout
                .sources
                .iter()
                .enumerate()
                .skip(row)
                .take(usize::from(self.view().scroll.height))
                .rev()
                .find_map(|(index, source)| {
                    source.item.as_ref().map(|item| (index, Arc::clone(item)))
                })
            {
                self.select_history_item(index, item, &layout);
            }
            return Some(vec![]);
        }
        if !self.view().items.active {
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => {
                self.active_view_mut().items.active = false;
                Some(vec![])
            }
            KeyCode::Up | KeyCode::Down => {
                let (layout, items) = self.visible_item_choices();
                let current = self
                    .view()
                    .items
                    .focus
                    .as_ref()
                    .and_then(|key| items.iter().position(|(_, item)| &item.key == key));
                let next = if key.code == KeyCode::Up {
                    current.unwrap_or(items.len()).saturating_sub(1)
                } else {
                    current.map_or(0, |i| (i + 1).min(items.len().saturating_sub(1)))
                };
                if let Some((row, item)) = items.get(next) {
                    self.select_history_item(*row, Arc::clone(item), &layout);
                }
                Some(vec![])
            }
            KeyCode::Enter | KeyCode::Char(' ') => Some(self.toggle_history_item()),
            KeyCode::Char('o') => Some(self.open_history_details(false, true)),
            KeyCode::Char('c') => Some(
                self.selected_history_item()
                    .map(|item| UiAction::Copy {
                        node_id: item.primary_node_id.clone(),
                        code_block: None,
                    })
                    .into_iter()
                    .collect(),
            ),
            // Function/global shortcuts keep their ordinary owner; plain text
            // cannot accidentally edit or send the hidden composer.
            KeyCode::F(_) => None,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => Some(vec![]),
            _ => None,
        }
    }
    pub(super) fn toggle_history_item(&mut self) -> Vec<UiAction> {
        let Some(item) = self.selected_history_item() else {
            return vec![];
        };
        if !item.expandable {
            return vec![];
        }
        let open =
            self.active_view_mut()
                .items
                .toggle(item.key.clone(), item.default_open, &item.outcome);
        self.dirty = true;
        if !open {
            self.active_view_mut().items.cancel_request(&item.key);
            return vec![];
        }
        self.request_tool_preview(&item).into_iter().collect()
    }
    fn request_tool_preview(&mut self, item: &ItemRegion) -> Option<UiAction> {
        if item.execution_id.is_none()
            || !self.view().items.needs_preview(&item.key, &item.outcome)
            || self.view().items.pending_previews() >= 3
        {
            return None;
        }
        let mut call = None;
        for node in self.view().nodes() {
            for (index, block) in node.presentation.blocks.iter().enumerate() {
                if let Some(tool) = &block.tool
                    && ItemKey::tool(tool, &node.node_id, index) == item.key
                {
                    call = Some((tool.group_id.clone(), tool.call_id.clone()));
                    break;
                }
            }
            if call.is_some() {
                break;
            }
        }
        let (group_id, call_id) = call?;
        let thread_id = self.view().thread_id.clone()?;
        let generation = self.view().generation;
        let request_revision = self
            .active_view_mut()
            .items
            .request(&item.key, &item.outcome);
        Some(UiAction::LoadToolPreview {
            thread_id,
            generation,
            key: item.key.clone(),
            request_revision,
            source_node_id: item.primary_node_id.clone(),
            group_id,
            call_id,
        })
    }
    pub(super) fn prepare_visible_tool_previews(
        &mut self,
        layout: &HistoryLayout,
        scroll: usize,
        height: u16,
    ) {
        let mut seen = BTreeSet::new();
        let items = layout
            .sources
            .iter()
            .skip(scroll)
            .take(usize::from(height))
            .filter_map(|source| source.item.as_ref())
            .filter(|item| seen.insert(item.key.clone()))
            .map(Arc::clone)
            .collect::<Vec<_>>();
        let keys = items
            .iter()
            .map(|item| item.key.clone())
            .collect::<Vec<_>>();
        self.active_view_mut().items.mark_visible(&keys);
        for item in items {
            if self
                .view()
                .items
                .needs_visible_preview(&item.key, &item.outcome)
                && let Some(action) = self.request_tool_preview(&item)
            {
                self.deferred_actions.push(action);
            }
        }
    }
    pub(super) fn open_history_details(&mut self, links: bool, original: bool) -> Vec<UiAction> {
        let Some(item) = self.selected_history_item() else {
            self.details = Some(details::Details::new(self.view(), links));
            return vec![];
        };
        let source = item.primary_node_id.clone();
        let loaded = self
            .view()
            .nodes()
            .iter()
            .any(|node| node.node_id == source && !node.presentation.body_evicted);
        let mut details = details::Details::for_node(self.view(), source.clone(), links)
            .with_sources(&item.source_node_ids);
        let generation = self.view().generation;
        let missing_link_source = links
            && item.source_node_ids.iter().any(|id| {
                !self
                    .view()
                    .nodes()
                    .iter()
                    .any(|node| &node.node_id == id && !node.presentation.body_evicted)
            });
        let action = if original || (!loaded && !links) {
            Some(details.open_source(source, &mut self.next_detail_request))
        } else if missing_link_source {
            self.view().thread_id.clone().map(|thread| {
                details.request_links(thread, generation, &mut self.next_detail_request)
            })
        } else {
            None
        };
        self.details = Some(details);
        action.into_iter().collect()
    }
    pub(super) fn history_click(&mut self, column: u16, row: u16) -> Vec<UiAction> {
        let Some(area) = self.view().scroll.area else {
            return vec![];
        };
        if !area
            .inner(ratatui::layout::Margin::new(1, 1))
            .contains(ratatui::layout::Position::new(column, row))
        {
            return vec![];
        }
        let layout = self.item_layout();
        let y = self.view().scroll.row + usize::from(row.saturating_sub(area.y + 1));
        let Some(item) = layout
            .sources
            .get(y)
            .and_then(|source| source.item.as_ref())
            .map(Arc::clone)
        else {
            return vec![];
        };
        self.select_history_item(y, item, &layout);
        self.toggle_history_item()
    }
    pub(super) fn accept_tool_preview(
        &mut self,
        thread: &str,
        generation: u64,
        key: ItemKey,
        request_revision: u64,
        result: Result<Preview, String>,
    ) {
        let budget = usize::try_from(self.config.cache.body_bytes)
            .unwrap_or(usize::MAX)
            .max(64 * 1024);
        if let Some(view) = self.view_for(thread, generation) {
            let outcome = view
                .nodes()
                .iter()
                .flat_map(|node| {
                    node.presentation
                        .blocks
                        .iter()
                        .enumerate()
                        .filter_map(move |(index, block)| {
                            block.tool.as_ref().map(|tool| (node, index, block, tool))
                        })
                })
                .filter(|(node, index, _, tool)| ItemKey::tool(tool, &node.node_id, *index) == key)
                .max_by_key(|(_, _, block, _)| {
                    u8::from(block.kind == crate::presentation::BlockKind::ToolResult)
                })
                .map(|(_, _, _, tool)| presentation::tool_outcome(tool));
            if outcome
                .as_deref()
                .is_none_or(|outcome| !view.items.matches_outcome(&key, outcome))
            {
                return;
            }
            let changed = match result {
                Ok(preview) => view
                    .items
                    .accept_preview(&key, request_revision, preview, budget),
                Err(message) => {
                    view.items
                        .accept_failed_preview(&key, request_revision, message, budget)
                }
            };
            self.dirty |= changed;
        }
    }
}

pub(super) fn highlight(
    frame: &mut Frame<'_>,
    area: Rect,
    history: &HistoryLayout,
    scroll: usize,
    view: &ThreadView,
    palette: theme::Palette,
) {
    if !view.items.active {
        return;
    }
    let Some(key) = &view.items.focus else {
        return;
    };
    for (line, source) in history
        .sources
        .iter()
        .skip(scroll)
        .take(usize::from(area.height.saturating_sub(2)))
        .enumerate()
    {
        if source.item.as_ref().is_some_and(|item| &item.key == key) {
            let row = Rect::new(
                area.x + 1,
                area.y + 1 + line as u16,
                area.width.saturating_sub(2),
                1,
            );
            frame.buffer_mut().set_style(row, palette.interactive());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn fixture() -> UiState {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes: vec![UiNode {
                node_id: "source".into(),
                role: "assistant".into(),
                text: "Visible original body".into(),
                total_chars: 21,
                ..Default::default()
            }],
            pending: Default::default(),
            read_revision: 1,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
        });
        Terminal::new(TestBackend::new(80, 28))
            .unwrap()
            .draw(|frame| draw(frame, &mut ui))
            .unwrap();
        ui
    }
    fn key(ui: &mut UiState, code: KeyCode) -> Vec<UiAction> {
        ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }
    #[test]
    fn history_focus_keeps_global_popups_and_exact_detail_target() {
        let mut ui = fixture();
        key(&mut ui, KeyCode::Tab);
        assert!(ui.view().items.active);
        key(&mut ui, KeyCode::F(8));
        assert!(ui.operations.is_some());
        key(&mut ui, KeyCode::Esc);
        assert!(ui.operations.is_none());
        key(&mut ui, KeyCode::F(6));
        assert_eq!(ui.details.as_ref().unwrap().node_id, "source");
        key(&mut ui, KeyCode::Esc);
        key(&mut ui, KeyCode::F(7));
        assert_eq!(ui.details.as_ref().unwrap().node_id, "source");
        key(&mut ui, KeyCode::Esc);
        assert!(key(&mut ui, KeyCode::Esc).is_empty());
        assert!(!ui.view().items.active);
    }
    #[test]
    fn focus_moves_within_viewport_without_reanchoring_after_history_refresh() {
        let mut ui = fixture();
        let mut nodes = (0..20)
            .map(|index| UiNode {
                node_id: format!("n{index}"),
                role: "assistant".into(),
                text: format!("N{index:02} first\nN{index:02} second"),
                total_chars: 21,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let generation = ui.view().generation;
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation,
            nodes: nodes.clone(),
            pending: Default::default(),
            read_revision: 2,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 20,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        let mut top = |ui: &mut UiState| {
            terminal.draw(|frame| draw(frame, ui)).unwrap();
            let buffer = terminal.backend().buffer();
            (1..79)
                .map(|column| buffer[(column, 2)].symbol())
                .collect::<String>()
        };
        let before = top(&mut ui);
        key(&mut ui, KeyCode::Tab);
        key(&mut ui, KeyCode::Up);
        assert_eq!(
            top(&mut ui),
            before,
            "focus inside the viewport must not move its top"
        );
        nodes.last_mut().unwrap().text.push_str("\nnew tail");
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation,
            nodes,
            pending: Default::default(),
            read_revision: 3,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 20,
        });
        assert_eq!(
            top(&mut ui),
            before,
            "refresh must preserve the viewport anchor, not selected item row"
        );
    }
    #[test]
    fn operation_failure_survives_passive_resize_and_clears_on_deliberate_input() {
        let mut ui = fixture();
        ui.view_mut().status = "Concrete failure".into();
        ui.view_mut().promote_status_notice(NoticeKind::Failure);
        ui.event(Event::Resize(81, 28));
        assert!(ui.view().operation_notice.is_some());
        key(&mut ui, KeyCode::F(1));
        assert!(ui.help_visible);
        assert!(ui.view().operation_notice.is_none());
    }
    #[test]
    fn independent_operation_failure_remains_visible_during_recovery() {
        let mut ui = fixture();
        ui.view_mut().activity.snapshot = Some(activity::ActivitySnapshot {
            phase: activity::ActivityPhase::Recovery,
            recovery: Some(activity::RecoverySnapshot {
                message: "Saved recovery fact".into(),
                ..Default::default()
            }),
            ..Default::default()
        });
        ui.view_mut().status = "New operation failed".into();
        ui.view_mut().promote_status_notice(NoticeKind::Failure);
        ui.apply(UiUpdate::Status {
            thread_id: Some("thread".into()),
            text: "Unrelated background status".into(),
        });
        let notice = ui.view().operation_notice.as_ref().unwrap();
        assert_eq!(notice.text, "New operation failed");
        assert_eq!(notice.kind, NoticeKind::Failure);
        key(&mut ui, KeyCode::Char('x'));
        assert!(ui.view().operation_notice.is_none());
    }
    #[test]
    fn tab_selects_visible_continuation_without_requiring_its_first_row() {
        let mut ui = fixture();
        let node = &mut ui.view_mut().nodes_mut_for_test()[0];
        node.text = (0..100).map(|n| format!("Long body line {n}\n")).collect();
        node.total_chars = node.text.chars().count();
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        assert!(ui.view().scroll.row > 0);
        key(&mut ui, KeyCode::Tab);
        assert!(ui.view().items.active);
        assert_eq!(ui.view().items.focus, Some(ItemKey::Node("source".into())));
    }
    #[test]
    fn click_uses_inner_history_rows_and_never_selection_mode() {
        let mut ui = fixture();
        let area = ui.view().scroll.area.unwrap();
        assert!(ui.history_click(area.x, area.y).is_empty());
        assert!(!ui.view().items.active);
        ui.selection_mode = true;
        ui.event(Event::Mouse(event::MouseEvent {
            kind: MouseEventKind::Down(event::MouseButton::Left),
            column: area.x + 1,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(!ui.view().items.active);
    }
}
