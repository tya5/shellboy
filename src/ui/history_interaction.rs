//! Ephemeral conversation focus and bounded explicit tool previews. No I/O.
use super::*;
use history_items::{ItemRegion, Preview};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoticeKind {
    Info,
    Attachment,
    Failure,
}
#[derive(Clone, Debug)]
pub(super) struct OperationNotice {
    pub text: String,
    pub kind: NoticeKind,
}

pub(super) fn key_bytes(key: &ItemKey) -> usize {
    match key {
        ItemKey::Node(id) | ItemKey::NoticeGroup(id) => id.capacity(),
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

fn is_send_progress_status(status: &str) -> bool {
    matches!(
        status,
        "送信要求を受け付け中"
            | "Accepting the send request"
            | "受付処理中です"
            | "Accepting the request"
    )
}

fn is_ready_status(status: &str) -> bool {
    matches!(status, "入力できます" | "Ready for input")
}

fn is_idle_status(status: &str) -> bool {
    matches!(
        status,
        "待機中"
            | "Waiting"
            | "処理中"
            | "Processing"
            | "完了"
            | "Complete"
            | "操作を準備しました"
            | "Operation is ready"
    )
}

impl ThreadView {
    pub(super) fn clear_operation_notice(&mut self) {
        if self.operation_notice.take().is_some() {
            self.status = self.state.clone();
        }
    }
    pub(super) fn promote_status_notice(&mut self, kind: NoticeKind) {
        // Send/receipt progress already has typed, reactive footer ownership.
        if kind != NoticeKind::Failure
            && self.sending().is_some()
            && is_send_progress_status(&self.status)
        {
            return;
        }
        if self.status.is_empty()
            || self.status == self.state
            || self.status == self.footer_phase().label()
            || is_ready_status(&self.status)
            || is_idle_status(&self.status)
            || self
                .operation_notice
                .as_ref()
                .is_some_and(|notice| notice.text == self.status)
        {
            return;
        }
        // Preserve failures and attachment warnings over unrelated background status.
        if self.operation_notice.as_ref().is_some_and(|notice| {
            (notice.kind == NoticeKind::Failure && kind != NoticeKind::Failure)
                || (notice.kind == NoticeKind::Attachment && kind == NoticeKind::Info)
        }) {
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
        let width = self.view().navigation.scroll.width;
        let height = self.view().navigation.scroll.height;
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
        let row = self.project_viewport(
            &layout,
            self.view().navigation.scroll.width,
            self.view().navigation.scroll.height,
        );
        layout
            .sources
            .iter()
            .skip(row)
            .take(usize::from(self.view().navigation.scroll.height))
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
        let width = self.view().navigation.scroll.width;
        let height = self.view().navigation.scroll.height;
        let current = self.project_viewport(layout, width, height);
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
        view.prepare_navigation();
        view.items.active = true;
        view.items.focus = Some(item.key.clone());
        view.history_focus_item = Some(item);
        let position = source.and_then(|source| {
            view.nodes().get(source.node).map(|node| {
                let anchor = ViewAnchor {
                    node_id: Some(node.node_id.clone()),
                    char_offset: source.offset,
                };
                let first = layout
                    .sources
                    .iter()
                    .position(|candidate| {
                        candidate.node == source.node && candidate.offset == source.offset
                    })
                    .unwrap_or(top);
                (anchor, top.saturating_sub(first))
            })
        });
        view.navigation.on_event(NavEvent::HistoryFocused {
            position,
            row: top,
            revision: view.history_revision(),
            origin: layout.sources.first().map_or(0, |source| source.node),
        });
        self.dirty = true;
    }
    pub(super) fn history_focus_key(&mut self, key: KeyEvent) -> Option<Vec<UiAction>> {
        let key = key_hints::readonly_key(key);
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
            let row = self.project_viewport(
                &layout,
                self.view().navigation.scroll.width,
                self.view().navigation.scroll.height,
            );
            if let Some((index, item)) = layout
                .sources
                .iter()
                .enumerate()
                .skip(row)
                .take(usize::from(self.view().navigation.scroll.height))
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
                if current.is_some_and(|index| {
                    (key.code == KeyCode::Up && index == 0)
                        || (key.code == KeyCode::Down && index + 1 == items.len())
                }) {
                    // Preserve an in-flight navigation read instead of reselecting
                    // the edge item and cancelling its pending movement.
                    return Some(self.scroll(if key.code == KeyCode::Up { -1 } else { 1 }));
                }
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
            KeyCode::Left | KeyCode::Right
                if self
                    .selected_history_item()
                    .is_some_and(|item| self.view().has_branch_marker(&item.primary_node_id)) =>
            {
                // Preview the next alternative at a branch marker (#237); never switches.
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                if let Some(item) = self.selected_history_item() {
                    self.active_view_mut()
                        .step_branch_preview(&item.primary_node_id, delta);
                    self.dirty = true;
                }
                Some(vec![])
            }
            KeyCode::Enter
                if self
                    .selected_history_item()
                    .and_then(|item| self.view().branch_target(&item.primary_node_id))
                    .is_some() =>
            {
                // A previewed branch opens its Thread (#237); the followed one keeps Enter's usual meaning.
                let target = self
                    .selected_history_item()
                    .and_then(|item| self.view().branch_target(&item.primary_node_id));
                Some(
                    target
                        .map(|thread| self.select_thread(thread))
                        .unwrap_or_default(),
                )
            }
            KeyCode::Enter => Some(
                if self
                    .selected_history_item()
                    .is_some_and(|item| matches!(item.key, ItemKey::NoticeGroup(_)))
                {
                    self.toggle_history_item()
                } else {
                    self.follow_history_notice()
                        .unwrap_or_else(|| self.toggle_history_item())
                },
            ),
            KeyCode::Char(' ') => Some(self.toggle_history_item()),
            code if key_hints::Action::Code.matches(code) => Some(self.choose_code(false)),
            code if key_hints::Action::Original.matches(code) => {
                Some(self.open_history_details(false, true))
            }
            code if key_hints::Action::Copy.matches(code) => Some(
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
    fn follow_history_notice(&mut self) -> Option<Vec<UiAction>> {
        let item = self.selected_history_item()?;
        let notice = self
            .view()
            .nodes()
            .iter()
            .find(|n| n.node_id == item.primary_node_id)
            .filter(|node| node.presentation.kind == crate::presentation::PresentationKind::Event)
            .map(|node| {
                let field = |key: &str| {
                    node.presentation
                        .fields
                        .iter()
                        .find(|f| f.key == key)
                        .map(|f| f.value.clone())
                };
                (
                    field("/ui_notice/execution_id").or_else(|| field("/payload/execution_id")),
                    field("/ui_notice/thread_id").or_else(|| field("/payload/thread_id")),
                    field("/ui_notice/source_node"),
                    field("/ui_notice/group_id"),
                    field("/ui_notice/call_id"),
                    field("/ui_notice/unavailable").map(|legacy| {
                        node.presentation
                            .ui_messages
                            .notice
                            .as_ref()
                            .map(|message| message.render(self.config.ui.language))
                            .unwrap_or(legacy)
                    }),
                    field("/ui_notice/source_error").map(|legacy| {
                        node.presentation
                            .ui_messages
                            .source_error
                            .map(|message| message.render(self.config.ui.language).to_owned())
                            .unwrap_or(legacy)
                    }),
                )
            });
        if let Some((execution, thread, source, group, call, unavailable, source_error)) = notice {
            if let Some(reason) = unavailable {
                self.active_view_mut().status = reason;
                self.active_view_mut()
                    .promote_status_notice(NoticeKind::Failure);
                self.dirty = true;
                return Some(vec![]);
            }
            if let Some(execution) = execution {
                let layout = self.item_layout();
                if let Some((row, target)) =
                    layout.sources.iter().enumerate().find_map(|(row, source)| {
                        source
                            .item
                            .as_ref()
                            .filter(|target| {
                                target.execution_id.as_ref() == Some(&execution)
                                    && matches!(
                                        target.key,
                                        ItemKey::Tool { .. } | ItemKey::LocalTool { .. }
                                    )
                            })
                            .map(|target| (row, Arc::clone(target)))
                    })
                {
                    self.select_history_item(row, target, &layout);
                    return Some(vec![]);
                }
                if let (Some(source), Some(group_id), Some(call_id)) = (source, group, call) {
                    let mut details =
                        details::Details::for_node(self.view(), source.clone(), false)
                            .with_sources(std::slice::from_ref(&source))
                            .with_item_key(ItemKey::Tool { group_id, call_id });
                    let action = details.open_source(source, &mut self.next_detail_request);
                    self.details = Some(details);
                    self.dirty = true;
                    return Some(vec![action]);
                }
                let language = self.config.ui.language;
                self.active_view_mut().status = source_error.unwrap_or_else(|| {
                    crate::presentation::messages::Text::SourceOutsideWindow
                        .render(language)
                        .into()
                });
                self.active_view_mut()
                    .promote_status_notice(NoticeKind::Failure);
                self.dirty = true;
                return Some(vec![]);
            }
            if let Some(thread) = thread {
                return Some(self.select_thread(thread));
            }
        }
        None
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
        self.request_tool_preview(&item, true).into_iter().collect()
    }
    fn request_tool_preview(&mut self, item: &ItemRegion, explicit: bool) -> Option<UiAction> {
        let needed = if explicit {
            self.view().items.needs_preview(&item.key, &item.outcome)
        } else {
            self.view()
                .items
                .needs_visible_preview(&item.key, &item.outcome, item.default_open)
        };
        if item.execution_id.is_none() || !needed || self.view().items.pending_previews() >= 3 {
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
        let limit_bytes = if explicit || self.view().items.is_open(&item.key, false, &item.outcome)
        {
            16 * 1024
        } else {
            2048
        };
        let request_revision = self
            .active_view_mut()
            .items
            .request(&item.key, &item.outcome);
        Some(UiAction::LoadToolPreview {
            thread_id,
            generation,
            key: item.key.clone(),
            request_revision,
            cancel: self
                .view()
                .items
                .preview_cancel(&item.key)
                .expect("registered preview request"),
            limit_bytes,
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
        let _ = self.active_view_mut().items.mark_visible(&keys);
        for item in &items {
            if self.view().items.preview_request(&item.key).is_some()
                && (!self
                    .view()
                    .items
                    .is_open(&item.key, item.default_open, &item.outcome)
                    || !self.view().items.preview_loading(&item.key, &item.outcome))
            {
                self.active_view_mut().items.cancel_request(&item.key);
            }
        }
        for item in items {
            if self
                .view()
                .items
                .needs_visible_preview(&item.key, &item.outcome, item.default_open)
                && let Some(action) = self.request_tool_preview(&item, false)
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
        let command_source = self
            .view()
            .describe_calls
            .then(|| {
                self.view()
                    .nodes()
                    .iter()
                    .flat_map(|node| {
                        node.presentation.blocks.iter().enumerate().filter_map(
                            move |(index, block)| {
                                block.tool.as_ref().map(|tool| (node, index, tool))
                            },
                        )
                    })
                    .find_map(|(node, index, tool)| {
                        (ItemKey::tool(tool, &node.node_id, index) == item.key
                            && crate::tool_description::label(tool.description.as_deref())
                                .is_some())
                        .then(|| tool.call_node_id.clone())
                        .flatten()
                    })
            })
            .flatten();
        let source = command_source.unwrap_or_else(|| item.primary_node_id.clone());
        let loaded = self
            .view()
            .nodes()
            .iter()
            .any(|node| node.node_id == source && !node.presentation.body_evicted);
        let mut details = details::Details::for_node(self.view(), source.clone(), links)
            .with_sources(&item.source_node_ids)
            .with_item_key(item.key.clone());
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
        } else if !links {
            Some(details.load_sender(&mut self.next_detail_request))
        } else {
            None
        };
        self.details = Some(details);
        action.into_iter().collect()
    }
    pub(super) fn history_click(&mut self, column: u16, row: u16) -> Vec<UiAction> {
        let Some(area) = self.view().navigation.scroll.area else {
            return vec![];
        };
        if !area
            .inner(ratatui::layout::Margin::new(1, 1))
            .contains(ratatui::layout::Position::new(column, row))
        {
            return vec![];
        }
        let layout = self.item_layout();
        let y = self.view().navigation.scroll.row() + usize::from(row.saturating_sub(area.y + 1));
        let Some(item) = layout
            .sources
            .get(y)
            .and_then(|source| source.item.as_ref())
            .map(Arc::clone)
        else {
            return vec![];
        };
        let click_target = match &item.key {
            ItemKey::Tool { .. } | ItemKey::LocalTool { .. } | ItemKey::NoticeGroup(_) => true,
            ItemKey::Node(node_id) => self.view().nodes().iter().any(|node| {
                &node.node_id == node_id
                    && node.presentation.kind == crate::presentation::PresentationKind::Event
                    && node.role != "received_response"
            }),
        };
        if !item.expandable || !click_target {
            return vec![];
        }
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
                .max_by_key(|(_, _, block, _)| presentation::tool_observation_rank(block))
                .map(|(_, _, _, tool)| presentation::tool_outcome(tool));
            if outcome
                .as_deref()
                .is_none_or(|outcome| !view.items.matches_outcome(&key, outcome))
            {
                // Retire only this matching in-flight token. A source/outcome
                // change cannot strand one of the three preview slots, and an
                // older reply must not cancel a newer request for the same key.
                self.dirty |= view.items.retire_request(&key, request_revision);
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
    // The cached projection is bounded; find the real header, not the first
    // visible continuation of an expanded item.
    let Some(header) = history
        .sources
        .iter()
        .position(|source| source.item.as_ref().is_some_and(|item| &item.key == key))
    else {
        return;
    };
    if header < scroll || header - scroll >= usize::from(area.height.saturating_sub(2)) {
        return;
    }
    let row = Rect::new(
        area.x + 1,
        area.y + 1 + (header - scroll) as u16,
        area.width.saturating_sub(2),
        1,
    );
    frame.buffer_mut().set_style(row, palette.history_focus());
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn fixture() -> UiState {
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut ui = UiState::new(&config, std::env::temp_dir());
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
    fn screen_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (buffer.area.y..buffer.area.bottom())
            .map(|y| {
                (buffer.area.x..buffer.area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn english_and_japanese_receipt_statuses_remain_non_notices() {
        for status in [
            "送信要求を受け付け中",
            "Accepting the send request",
            "受付処理中です",
            "Accepting the request",
        ] {
            assert!(is_send_progress_status(status), "{status}");
        }
        assert!(is_ready_status("入力できます"));
        assert!(is_ready_status("Ready for input"));
        for status in [
            "待機中",
            "Waiting",
            "処理中",
            "Processing",
            "完了",
            "Complete",
            "操作を準備しました",
            "Operation is ready",
        ] {
            assert!(is_idle_status(status), "{status}");
        }
        assert!(!is_idle_status("Provider supplied status"));
        assert!(!is_send_progress_status("Provider supplied error"));
        assert!(!is_ready_status("Provider supplied status"));
    }

    #[test]
    fn notification_enter_follows_exact_loaded_tool_without_fetching_output() {
        let mut ui = fixture();
        let tool = crate::presentation::ToolPresentation {
            group_id: Some("group".into()),
            call_id: "call".into(),
            execution_id: Some("execution".into()),
            status: "completed".into(),
            command: "fixture".into(),
            ..Default::default()
        };
        let fields = crate::presentation::fields_of_value(
            &serde_json::json!({"kind":"shell_ended", "payload":{"execution_id":"execution"}}),
        );
        let notice = UiNode {
            node_id: "notice".into(),
            presentation: crate::presentation::NodePresentation {
                kind: crate::presentation::PresentationKind::Event,
                title: "saved shell notice".into(),
                fields,
                ..Default::default()
            },
            ..Default::default()
        };
        let nodes = vec![
            UiNode {
                node_id: "source".into(),
                presentation: crate::presentation::NodePresentation {
                    kind: crate::presentation::PresentationKind::ToolResult,
                    blocks: vec![crate::presentation::PresentationBlock {
                        kind: crate::presentation::BlockKind::ToolResult,
                        tool: Some(tool),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            notice.clone(),
        ];
        let apply = |ui: &mut UiState, nodes: Vec<UiNode>, revision| {
            let count = nodes.len();
            ui.apply(UiUpdate::History {
                thread_id: "thread".into(),
                generation: ui.view().generation,
                nodes,
                pending: Default::default(),
                read_revision: revision,
                before_cursor: None,
                append: false,
                first_ordinal: 0,
                selection_revision: revision,
                total_nodes: count as u64,
            });
            Terminal::new(TestBackend::new(80, 28))
                .unwrap()
                .draw(|frame| draw(frame, ui))
                .unwrap();
        };
        apply(&mut ui, nodes, 2);
        key(&mut ui, KeyCode::Tab);
        assert_eq!(ui.view().items.focus, Some(ItemKey::Node("notice".into())));
        key(&mut ui, KeyCode::Char(' '));
        assert_eq!(
            ui.view().items.focus,
            Some(ItemKey::Node("notice".into())),
            "Space only folds; Enter follows"
        );
        assert!(key(&mut ui, KeyCode::Enter).is_empty());
        assert_eq!(
            ui.view().items.focus,
            Some(ItemKey::Tool {
                group_id: "group".into(),
                call_id: "call".into()
            })
        );
        ui = fixture();
        apply(&mut ui, vec![notice.clone()], 3);
        key(&mut ui, KeyCode::Tab);
        assert!(key(&mut ui, KeyCode::Enter).is_empty());
        assert!(ui.view().status.contains("表示範囲外"));
        assert_eq!(ui.view().items.focus, Some(ItemKey::Node("notice".into())));
        ui = fixture();
        let mut child = notice;
        child.presentation.fields = crate::presentation::fields_of_value(
            &serde_json::json!({"kind":"child_turn_completed","payload":{"thread_id":"child"}}),
        );
        apply(&mut ui, vec![child], 4);
        key(&mut ui, KeyCode::Tab);
        key(&mut ui, KeyCode::Enter);
        assert_eq!(ui.view().thread_id.as_deref(), Some("child"));
    }
    #[test]
    fn single_notice_enter_and_copy_target_original_in_both_languages() {
        for language in [crate::config::UiLanguage::Ja, crate::config::UiLanguage::En] {
            let mut ui = fixture();
            ui.config.ui.language = language;
            ui.view_mut().language = language;
            let mut fields = crate::presentation::fields_of_value(
                &serde_json::json!({"kind":"shell_ended", "payload":{"execution_id":"missing"}}),
            );
            fields.push(crate::presentation::PresentationField {
                key: "/ui_notice/source_node".into(),
                value: "original".into(),
                ..Default::default()
            });
            for (key, value) in [("group_id", "group"), ("call_id", "call")] {
                fields.push(crate::presentation::PresentationField {
                    key: format!("/ui_notice/{key}"),
                    value: value.into(),
                    ..Default::default()
                });
            }
            let notice = UiNode {
                node_id: "notice".into(),
                role: "event".into(),
                text: "canonical notice text".into(),
                presentation: crate::presentation::NodePresentation {
                    kind: crate::presentation::PresentationKind::Event,
                    fields,
                    ..Default::default()
                },
                ..Default::default()
            };
            ui.apply(UiUpdate::History {
                thread_id: "thread".into(),
                generation: ui.view().generation,
                nodes: vec![notice],
                pending: Default::default(),
                read_revision: 2,
                before_cursor: None,
                append: false,
                first_ordinal: 0,
                selection_revision: 2,
                total_nodes: 1,
            });
            Terminal::new(TestBackend::new(80, 28))
                .unwrap()
                .draw(|frame| draw(frame, &mut ui))
                .unwrap();
            key(&mut ui, KeyCode::Tab);
            assert_eq!(ui.view().items.focus, Some(ItemKey::Node("notice".into())));
            let actions = key(&mut ui, KeyCode::Char('c'));
            assert!(
                matches!(actions.as_slice(), [UiAction::Copy { node_id, code_block: None }] if node_id == "notice")
            );
            let actions = key(&mut ui, KeyCode::Enter);
            assert!(
                matches!(actions.as_slice(), [UiAction::LoadOriginal { node_id, .. }] if node_id == "original")
            );
            assert_eq!(ui.view().nodes()[0].text, "canonical notice text");
        }
    }

    #[test]
    fn expanded_notice_group_preserves_each_original_and_enter_target() {
        let mut ui = fixture();
        let notice = |id: &str, source: &str| {
            let mut fields = crate::presentation::fields_of_value(
                &serde_json::json!({"kind":"shell_ended", "payload":{"execution_id":id}}),
            );
            fields.extend(
                [
                    ("execution_id", id),
                    ("group_id", "group"),
                    ("call_id", id),
                    ("source_node", source),
                ]
                .into_iter()
                .map(|(key, value)| crate::presentation::PresentationField {
                    key: format!("/ui_notice/{key}"),
                    value: value.into(),
                    ..Default::default()
                }),
            );
            UiNode {
                node_id: id.into(),
                role: "event".into(),
                text: format!("original-{id}"),
                total_chars: 12,
                presentation: crate::presentation::NodePresentation {
                    kind: crate::presentation::PresentationKind::Event,
                    fields,
                    ..Default::default()
                },
                ..Default::default()
            }
        };
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes: vec![notice("a", "original-a"), notice("b", "original-b")],
            pending: Default::default(),
            read_revision: 2,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 2,
            total_nodes: 2,
        });
        Terminal::new(TestBackend::new(80, 28))
            .unwrap()
            .draw(|frame| draw(frame, &mut ui))
            .unwrap();
        key(&mut ui, KeyCode::Tab);
        assert_eq!(
            ui.view().items.focus,
            Some(ItemKey::NoticeGroup("a".into()))
        );
        assert!(key(&mut ui, KeyCode::Enter).is_empty());
        assert!(
            ui.view()
                .items
                .is_open(&ItemKey::NoticeGroup("a".into()), false, "")
        );
        key(&mut ui, KeyCode::Down);
        assert_eq!(ui.view().items.focus, Some(ItemKey::Node("a".into())));
        key(&mut ui, KeyCode::Down);
        assert_eq!(ui.view().items.focus, Some(ItemKey::Node("b".into())));
        let actions = key(&mut ui, KeyCode::Enter);
        assert!(
            matches!(actions.as_slice(), [UiAction::LoadOriginal { node_id, .. }] if node_id == "original-b")
        );
    }

    fn branch_ui(language: crate::config::UiLanguage) -> UiState {
        use crate::presentation::branch_marker::{Alt, BranchMarker};
        let mut config = Config::default();
        config.ui.language = language;
        let mut ui = UiState::new(&config, std::env::temp_dir());
        ui.select_thread("thread".into());
        let alt = |id: &str, text: &str| Alt {
            thread_id: id.into(),
            excerpt: Some(text.into()),
        };
        let body = "answer before the fork".to_owned();
        let mut presentation = crate::presentation::project(
            "assistant_response",
            &serde_json::json!({"output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "answer before the fork"}]}]}),
            &body,
            0,
        );
        presentation.branch_marker = Some(BranchMarker {
            alternatives: vec![
                alt("first", "fix the tests"),
                alt("thread", "make CI pass"),
                alt("third", "try with mocks"),
            ],
            current: 1,
            preview: 1,
        });
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes: vec![UiNode {
                node_id: "anchor".into(),
                role: "assistant_response".into(),
                text: body,
                total_chars: 22,
                presentation,
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
        ui
    }
    fn screen_at(ui: &mut UiState, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|frame| draw(frame, ui)).unwrap();
        screen_text(&terminal)
    }
    #[test]
    fn branch_marker_row_follows_the_arrow_keys_and_enter_opens_the_previewed_thread() {
        use crate::config::UiLanguage;
        // Wide characters leave continuation cells; compare without spaces.
        let squash = |text: String| text.replace(['\n', '\u{a0}', ' '], "");
        for (language, current, branch) in [
            (UiLanguage::En, "current", "branch"),
            (UiLanguage::Ja, "今の会話", "別の会話"),
        ] {
            let mut ui = branch_ui(language);
            for width in [100u16, 40] {
                let shown = screen_at(&mut ui, width);
                println!("--- {language:?} inline marker, {width} columns ---\n{shown}");
                let flat = squash(shown.clone());
                assert!(flat.contains("↳2/3⋯"), "{shown}");
                assert!(
                    flat.contains(current) && flat.contains("\"makeCIpass\""),
                    "{shown}"
                );
            }
            // ←/→ need the history item to be selected first, and never switch Threads.
            assert!(key(&mut ui, KeyCode::Right).is_empty());
            assert_eq!(
                ui.view().nodes()[0]
                    .presentation
                    .branch_marker
                    .as_ref()
                    .unwrap()
                    .preview,
                1
            );
            key(&mut ui, KeyCode::Tab);
            assert!(ui.view().items.active);
            assert!(key(&mut ui, KeyCode::Right).is_empty());
            let shown = squash(screen_at(&mut ui, 100));
            assert!(
                shown.contains("↳3/3⋯") && shown.contains("trywithmocks"),
                "{shown}"
            );
            assert!(shown.contains(branch), "{shown}");
            key(&mut ui, KeyCode::Right);
            let shown = squash(screen_at(&mut ui, 100));
            assert!(
                shown.contains("↳1/3⋯") && shown.contains("fixthetests"),
                "wraps: {shown}"
            );
            key(&mut ui, KeyCode::Left);
            assert!(squash(screen_at(&mut ui, 100)).contains("↳3/3⋯"));
            assert_eq!(
                ui.view().thread_id.as_deref(),
                Some("thread"),
                "previewing never switches"
            );
            let actions = key(&mut ui, KeyCode::Enter);
            assert!(
                actions.iter().any(|a| matches!(a, UiAction::SelectThread { thread_id, .. } if thread_id == "third"))
                    || ui.view().thread_id.as_deref() == Some("third"),
                "Enter opens the previewed branch"
            );
        }
    }
    #[test]
    fn enter_on_the_followed_alternative_keeps_its_usual_meaning() {
        let mut ui = branch_ui(crate::config::UiLanguage::En);
        key(&mut ui, KeyCode::Tab);
        let before = ui.view().thread_id.clone();
        key(&mut ui, KeyCode::Enter);
        assert_eq!(ui.view().thread_id, before);
        assert_eq!(
            ui.view().nodes()[0]
                .presentation
                .branch_marker
                .as_ref()
                .unwrap()
                .preview,
            1
        );
    }
    #[test]
    fn previewing_changes_the_render_stamp_so_cached_rows_are_rebuilt() {
        let mut ui = branch_ui(crate::config::UiLanguage::En);
        ui.active_view_mut().nodes_mut_for_test()[0]
            .presentation
            .projection_stamp = Some([7; 32]);
        let before = ui.view().nodes()[0].presentation.projection_stamp;
        assert!(ui.active_view_mut().step_branch_preview("anchor", 1));
        assert_ne!(before, ui.view().nodes()[0].presentation.projection_stamp);
        assert!(!ui.active_view_mut().step_branch_preview("missing", 1));
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
    fn stale_outcome_releases_its_preview_slot_without_cancelling_a_new_request() {
        use crate::presentation::{BlockKind, PresentationBlock, ToolPresentation};
        let mut ui = fixture();
        let tool = ToolPresentation {
            group_id: Some("g".into()),
            call_id: "c".into(),
            status: "completed".into(),
            exit_code: Some(0),
            ..Default::default()
        };
        let key = ItemKey::tool(&tool, "source", 0);
        let current_outcome = presentation::tool_outcome(&tool);
        ui.view_mut().nodes_mut_for_test()[0].presentation.blocks = vec![PresentationBlock {
            kind: BlockKind::ToolResult,
            tool: Some(tool),
            ..Default::default()
        }];
        let old = ui.view_mut().items.request(&key, "running:None");
        assert_eq!(ui.view().items.pending_previews(), 1);
        let generation = ui.view().generation;
        ui.accept_tool_preview(
            "thread",
            generation,
            key.clone(),
            old,
            Ok(Preview::default()),
        );
        assert_eq!(
            ui.view().items.pending_previews(),
            0,
            "stale source must release its own slot"
        );
        let next = ui.view_mut().items.request(&key, &current_outcome);
        ui.accept_tool_preview(
            "thread",
            generation,
            key.clone(),
            old,
            Ok(Preview::default()),
        );
        assert_eq!(
            ui.view().items.pending_previews(),
            1,
            "an older token cannot cancel its replacement"
        );
        ui.accept_tool_preview("thread", generation, key, next, Ok(Preview::default()));
        assert_eq!(ui.view().items.pending_previews(), 0);
        assert_eq!(ui.view().nodes()[0].text, "Visible original body");
    }
    #[test]
    fn rejected_preview_action_releases_its_pending_slot() {
        use crate::presentation::{BlockKind, PresentationBlock, ToolPresentation};
        let mut ui = fixture();
        let tool = ToolPresentation {
            group_id: Some("g".into()),
            call_id: "c".into(),
            status: "completed".into(),
            exit_code: Some(0),
            ..Default::default()
        };
        let key = ItemKey::tool(&tool, "source", 0);
        let outcome = presentation::tool_outcome(&tool);
        ui.view_mut().nodes_mut_for_test()[0].presentation.blocks = vec![PresentationBlock {
            kind: BlockKind::ToolResult,
            tool: Some(tool),
            ..Default::default()
        }];
        let request_revision = ui.view_mut().items.request(&key, &outcome);
        let action = UiAction::LoadToolPreview {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            key: key.clone(),
            request_revision,
            cancel: ui.view().items.preview_cancel(&key).unwrap(),
            limit_bytes: 16384,
            source_node_id: "source".into(),
            group_id: Some("g".into()),
            call_id: "c".into(),
        };
        ui.action_rejected(&action);
        assert_eq!(ui.view().items.pending_previews(), 0);
        assert!(ui.view().items.preview_failed(&key, &outcome));
        assert_eq!(ui.view().nodes()[0].text, "Visible original body");
    }
    #[test]
    fn tab_selects_visible_continuation_without_requiring_its_first_row() {
        let mut ui = fixture();
        let node = &mut ui.view_mut().nodes_mut_for_test()[0];
        node.text = (0..100).map(|n| format!("Long body line {n}\n")).collect();
        node.total_chars = node.text.chars().count();
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        assert!(ui.view().navigation.scroll.row() > 0);
        key(&mut ui, KeyCode::Tab);
        assert!(ui.view().items.active);
        assert_eq!(ui.view().items.focus, Some(ItemKey::Node("source".into())));
    }
    fn click_item_row(ui: &mut UiState, key: &ItemKey) {
        let layout = ui.item_layout();
        let source_row = layout
            .sources
            .iter()
            .position(|source| source.item.as_ref().is_some_and(|item| &item.key == key))
            .expect("clickable item has a history row");
        let area = ui.view().navigation.scroll.area.unwrap();
        let first_visible = ui.view().navigation.scroll.row();
        assert!(source_row >= first_visible);
        let row = area.y + 1 + u16::try_from(source_row - first_visible).unwrap();
        assert!(
            area.inner(ratatui::layout::Margin::new(1, 1))
                .contains(ratatui::layout::Position::new(area.x + 1, row))
        );
        ui.history_click(area.x + 1, row);
    }

    #[test]
    fn clicks_on_user_and_model_message_rows_do_not_select_or_toggle() {
        for (role, kind) in [
            ("user", crate::presentation::PresentationKind::User),
            (
                "assistant",
                crate::presentation::PresentationKind::Assistant,
            ),
            (
                "received_response",
                crate::presentation::PresentationKind::Event,
            ),
        ] {
            let mut ui = fixture();
            let node = &mut ui.view_mut().nodes_mut_for_test()[0];
            node.role = role.into();
            node.presentation.kind = kind;
            let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();

            let key = ItemKey::Node("source".into());
            let layout = ui.item_layout();
            assert!(
                layout
                    .sources
                    .iter()
                    .any(|source| { source.item.as_ref().is_some_and(|item| item.key == key) })
            );
            assert!(!ui.view().items.active);
            click_item_row(&mut ui, &key);
            assert!(!ui.view().items.active, "{role} row click should be inert");
            assert_eq!(ui.view().items.focus, None);
        }
    }

    #[test]
    fn clicks_on_tool_and_grouped_notice_rows_still_toggle() {
        let mut ui = fixture();
        let tool = crate::presentation::ToolPresentation {
            group_id: Some("group".into()),
            call_id: "call".into(),
            command: "printf synthetic".into(),
            description: Some("Inspect repository".into()),
            status: "completed".into(),
            call_node_id: Some("source".into()),
            ..Default::default()
        };
        let tool_key = ItemKey::tool(&tool, "source", 0);
        let tool_outcome = super::presentation::tool_outcome(&tool);
        {
            let node = &mut ui.view_mut().nodes_mut_for_test()[0];
            node.presentation.kind = crate::presentation::PresentationKind::Assistant;
            node.presentation.blocks = vec![crate::presentation::PresentationBlock {
                kind: crate::presentation::BlockKind::ToolCall,
                tool: Some(tool),
                ..Default::default()
            }];
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let collapsed = screen_text(&terminal);
        assert!(collapsed.contains("Inspect repository"), "{collapsed}");
        assert!(!collapsed.contains("printf synthetic"), "{collapsed}");
        click_item_row(&mut ui, &tool_key);
        assert!(ui.view().items.is_open(&tool_key, false, &tool_outcome));
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let expanded = screen_text(&terminal);
        assert!(expanded.contains("Inspect repository"), "{expanded}");
        assert!(expanded.contains("printf synthetic"), "{expanded}");
        click_item_row(&mut ui, &tool_key);
        assert!(!ui.view().items.is_open(&tool_key, false, &tool_outcome));
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let collapsed = screen_text(&terminal);
        assert!(collapsed.contains("Inspect repository"), "{collapsed}");
        assert!(!collapsed.contains("printf synthetic"), "{collapsed}");

        let local_tool = crate::presentation::ToolPresentation {
            command: "printf local".into(),
            status: "completed".into(),
            ..Default::default()
        };
        let local_key = ItemKey::tool(&local_tool, "source", 0);
        let local_outcome = super::presentation::tool_outcome(&local_tool);
        ui.view_mut().nodes_mut_for_test()[0].presentation.blocks[0].tool = Some(local_tool);
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        click_item_row(&mut ui, &local_key);
        assert!(ui.view().items.is_open(&local_key, false, &local_outcome));

        fn event(id: &str, kind: &str) -> UiNode {
            UiNode {
                node_id: id.into(),
                text: format!("raw-{id}"),
                total_chars: 5,
                presentation: crate::presentation::NodePresentation {
                    kind: crate::presentation::PresentationKind::Event,
                    ui_messages: crate::presentation::messages::PresentationMessages {
                        title: Some(crate::presentation::messages::Message::EventTitle {
                            kind: kind.into(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            }
        }

        let mut ui = fixture();
        *ui.view_mut().nodes_mut_for_test() = vec![
            event("notice-one", "run_started"),
            event("notice-two", "index_state"),
        ];
        ui.view_mut().total_nodes = 2;
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let notice_key = ItemKey::NoticeGroup("notice-one".into());
        click_item_row(&mut ui, &notice_key);
        assert!(ui.view().items.is_open(&notice_key, false, ""));

        let mut ui = fixture();
        *ui.view_mut().nodes_mut_for_test() = vec![event("single-notice", "run_started")];
        ui.view_mut().total_nodes = 1;
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let notice_key = ItemKey::Node("single-notice".into());
        assert!(ui.view().items.is_open(&notice_key, true, ""));
        click_item_row(&mut ui, &notice_key);
        assert!(!ui.view().items.is_open(&notice_key, true, ""));
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        click_item_row(&mut ui, &notice_key);
        assert!(ui.view().items.is_open(&notice_key, true, ""));
    }

    #[test]
    fn keyboard_toggles_tool_command_preview_with_its_row() {
        let mut ui = fixture();
        let tool = crate::presentation::ToolPresentation {
            group_id: Some("group".into()),
            call_id: "call".into(),
            command: "printf keyboard".into(),
            description: Some("Inspect repository".into()),
            status: "completed".into(),
            call_node_id: Some("source".into()),
            ..Default::default()
        };
        let tool_key = ItemKey::tool(&tool, "source", 0);
        let outcome = super::presentation::tool_outcome(&tool);
        ui.view_mut().nodes_mut_for_test()[0].presentation.blocks =
            vec![crate::presentation::PresentationBlock {
                kind: crate::presentation::BlockKind::ToolCall,
                tool: Some(tool),
                ..Default::default()
            }];
        let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let collapsed = screen_text(&terminal);
        assert!(collapsed.contains("Inspect repository"), "{collapsed}");
        assert!(!collapsed.contains("printf keyboard"), "{collapsed}");

        key(&mut ui, KeyCode::Tab);
        assert_eq!(ui.view().items.focus, Some(tool_key.clone()));
        key(&mut ui, KeyCode::Char(' '));
        assert!(ui.view().items.is_open(&tool_key, false, &outcome));
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let expanded = screen_text(&terminal);
        assert!(expanded.contains("Inspect repository"), "{expanded}");
        assert!(expanded.contains("printf keyboard"), "{expanded}");

        key(&mut ui, KeyCode::Char(' '));
        assert!(!ui.view().items.is_open(&tool_key, false, &outcome));
        terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
        let collapsed = screen_text(&terminal);
        assert!(collapsed.contains("Inspect repository"), "{collapsed}");
        assert!(!collapsed.contains("printf keyboard"), "{collapsed}");
    }

    #[test]
    fn click_uses_inner_history_rows_and_never_selection_mode() {
        let mut ui = fixture();
        let area = ui.view().navigation.scroll.area.unwrap();
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
    #[test]
    fn notification_outside_window_opens_only_its_exact_saved_original() {
        let mut ui = fixture();
        let fields = [
            ("execution_id", "execution"),
            ("group_id", "exact-group"),
            ("call_id", "same"),
            ("source_node", "exact-result"),
            ("title", "saved shell | printf CORRECT"),
        ]
        .into_iter()
        .map(|(key, value)| crate::presentation::PresentationField {
            key: format!("/ui_notice/{key}"),
            label: key.into(),
            value: value.into(),
            ..Default::default()
        })
        .collect();
        ui.apply(UiUpdate::History {
            thread_id: "thread".into(),
            generation: ui.view().generation,
            nodes: vec![UiNode {
                node_id: "notice".into(),
                presentation: crate::presentation::NodePresentation {
                    kind: crate::presentation::PresentationKind::Event,
                    title: "saved shell".into(),
                    fields,
                    ..Default::default()
                },
                ..Default::default()
            }],
            pending: Default::default(),
            read_revision: 2,
            before_cursor: None,
            append: false,
            first_ordinal: 0,
            selection_revision: 2,
            total_nodes: 1,
        });
        Terminal::new(TestBackend::new(80, 28))
            .unwrap()
            .draw(|frame| draw(frame, &mut ui))
            .unwrap();
        key(&mut ui, KeyCode::Tab);
        let actions = key(&mut ui, KeyCode::Enter);
        assert!(
            matches!(actions.as_slice(),[UiAction::LoadOriginal{node_id,..}] if node_id=="exact-result")
        );
        assert!(ui.details.is_some());
        assert_eq!(ui.view().thread_id.as_deref(), Some("thread"));
    }
}

#[cfg(test)]
mod heading_focus_tests {
    use super::*;
    use crate::config::PaletteMode;
    use ratatui::{Terminal, backend::TestBackend, style::Modifier};
    #[test]
    fn only_heading_is_styled_without_moving_text_or_styling_scrolled_continuations() {
        let key = ItemKey::Node("one".into());
        let region = Arc::new(ItemRegion {
            key: key.clone(),
            source_node_ids: vec!["one".into()],
            primary_node_id: "one".into(),
            execution_id: None,
            expandable: true,
            default_open: true,
            outcome: String::new(),
        });
        let history = HistoryLayout {
            text: String::new(),
            styled: vec![],
            lines: 3,
            targets: vec![],
            sources: (0..3)
                .map(|offset| presentation::RowSource {
                    node: 0,
                    covered_end: 1,
                    offset,
                    item: Some(region.clone()),
                })
                .collect(),
        };
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.items.active = true;
        view.items.focus = Some(key);
        for mode in [PaletteMode::Dark, PaletteMode::Light] {
            for (plain, rich) in [(false, true), (true, true), (false, false)] {
                let palette = theme::Palette::from_capabilities(mode, plain, rich);
                if !plain && rich {
                    assert_ne!(
                        palette.history_focus().bg,
                        palette
                            .user_band(crate::presentation::InputSource::Human)
                            .bg
                    );
                }
                for scroll in [0, 1] {
                    let mut terminal = Terminal::new(TestBackend::new(32, 8)).unwrap();
                    terminal
                        .draw(|frame| {
                            frame.render_widget(
                                Paragraph::new("HEADER\nOUTPUT\nNOTICE"),
                                Rect::new(1, 1, 30, 3),
                            );
                            highlight(
                                frame,
                                Rect::new(0, 0, 32, 8),
                                &history,
                                scroll,
                                &view,
                                palette,
                            );
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    assert_eq!(buffer[(1, 1)].symbol(), "H");
                    assert_eq!(buffer[(1, 2)].symbol(), "O");
                    assert_eq!(buffer[(1, 2)].bg, ratatui::style::Color::Reset);
                    assert!(
                        !buffer[(1, 2)]
                            .modifier
                            .intersects(Modifier::BOLD | Modifier::UNDERLINED)
                    );
                    assert!(!buffer[(1, 1)].modifier.contains(Modifier::UNDERLINED));
                    if scroll == 0 {
                        assert_eq!(
                            buffer[(1, 1)].bg,
                            palette
                                .history_focus()
                                .bg
                                .unwrap_or(ratatui::style::Color::Reset)
                        );
                        assert_eq!(
                            buffer[(1, 1)].modifier.contains(Modifier::BOLD),
                            plain || !rich
                        );
                    } else {
                        assert_eq!(buffer[(1, 1)].bg, ratatui::style::Color::Reset);
                        assert!(!buffer[(1, 1)].modifier.contains(Modifier::BOLD));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod description_original_identity_tests {
    use super::*;
    use crate::presentation::{
        BlockKind, NodePresentation, PresentationBlock, PresentationKind, ToolPresentation,
    };
    #[test]
    fn described_original_targets_exact_call_source_not_adjacent_result() {
        let mut ui = UiState::new(&crate::config::Config::default(), std::env::temp_dir());
        ui.active_view_mut().navigation.scroll.width = 80;
        ui.active_view_mut().navigation.scroll.height = 24;
        let tools = ["a", "b"].map(|call| ToolPresentation {
            group_id: Some("g".into()),
            call_id: call.into(),
            command: format!("ACTUAL-{call}"),
            description: Some(format!("LABEL-{call}")),
            call_node_id: Some(format!("call-{call}")),
            result_node_id: Some(format!("result-{call}")),
            status: "completed".into(),
            exit_code: Some(0),
            ..Default::default()
        });
        *ui.active_view_mut().nodes_mut_for_test() = tools
            .iter()
            .map(|tool| UiNode {
                node_id: tool.call_node_id.clone().unwrap(),
                role: "assistant".into(),
                presentation: NodePresentation {
                    kind: PresentationKind::Assistant,
                    blocks: vec![PresentationBlock {
                        kind: BlockKind::ToolCall,
                        tool: Some(tool.clone()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            })
            .collect();
        for tool in &tools {
            let key = ItemKey::tool(tool, tool.call_node_id.as_ref().unwrap(), 0);
            ui.active_view_mut().items.active = true;
            ui.active_view_mut().items.focus = Some(key);
            let actions = ui.open_history_details(false, true);
            assert_eq!(actions.len(), 1);
            assert!(
                matches!(&actions[0], UiAction::LoadOriginal { node_id, cursor: None, .. }
                if Some(node_id) == tool.call_node_id.as_ref())
            );
        }
    }
}
