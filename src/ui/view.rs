//! One Thread generation's source windows and navigation. No I/O or new workers.
use super::*;

mod navigation;
pub use navigation::Navigation;
pub(super) use navigation::{NavEvent, ReadEvent};

#[derive(Debug)]
pub struct ThreadView {
    pub(super) language: crate::config::UiLanguage,
    pub(super) palette: theme::Palette,
    pub(super) describe_calls: bool,
    pub(super) items: history_items::ItemState,
    pub(super) history_focus_item: Option<Arc<history_items::ItemRegion>>,
    pub(super) operation_notice: Option<history_interaction::OperationNotice>,
    pub(super) activity: activity::ActivityView,
    pub(super) live_work: live_work::LiveWorkView,
    pub(super) held_input: region::RegionState<held_input::HeldKey>,
    pub thread_id: Option<String>,
    pub cwd: PathBuf,
    pub agents_path: String,
    pub(super) selecting_agents: Option<String>,
    pub connection: Option<String>,
    pub model: Option<String>,
    /// Latest saved provider-reported usage; replaced only by an adopted snapshot.
    pub(super) context_usage: ContextUsage,
    pub effort: Option<String>,
    /// This Thread's own proactive-compaction threshold, if it set one (#229).
    pub(super) auto_compact_at: Option<u64>,
    pub title: String,
    pub state: String,
    pub draft: DraftSnapshot,
    pub generation: u64,
    nodes: Vec<UiNode>,
    pub(super) provisional_nodes: BTreeSet<String>,
    // At most one saved correspondence per retained provisional node.
    committed_nodes: BTreeMap<String, String>,
    pub before_cursor: Option<String>,
    pub first_ordinal: u64,
    pub selection_revision: u64,
    // Same-path retries add audit nodes without changing selection_revision.
    // Retain only the last accepted existing DB root-revision watermark.
    pub(super) received_revision: u64,
    // Last applied saved-history window, distinct from newer input requests.
    pub(super) received_window_revision: Option<u64>,
    pub total_nodes: u64,
    pub status: String,
    pub pending: BTreeMap<u64, PendingSend>,
    pub pending_messages: PendingMessages,
    pub delivery_read_revision: u64,
    pub(super) tool_refresh: Option<u64>,
    pub(super) tool_refresh_dirty: bool,
    pub(super) tool_observation_scope: Option<(u64, Vec<String>)>,
    tool_observation_clock: u64,
    tool_observation_checked: Option<(u64, u64)>,
    pub save_retry: bool,
    pub(super) draft_restore_pending: bool,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub tool_calls: u64,
    pub cancelled_calls: u64,
    pub counts_capped: bool,
    pub reasoning_expanded: BTreeSet<String>,
    history_revision: u64,
    body_storage_bytes: std::cell::Cell<Option<usize>>,
    pub navigation: Navigation,
    pub(super) composer: composer::Composer,
}
impl ThreadView {
    pub(super) fn draft_attachment_retention(&self) -> UiAction {
        // The next composer's deletions cannot release a pending/restoreable
        // send's images. Both local owners use the existing draft-image lease.
        let attachment_ids = self
            .draft
            .attachments
            .iter()
            .chain(
                self.pending
                    .values()
                    .filter(|pending| {
                        pending.send.is_some() && pending.snapshot.draft_id == self.draft.draft_id
                    })
                    .flat_map(|pending| pending.snapshot.attachments.iter()),
            )
            .map(|attachment| attachment.id.clone())
            .collect::<BTreeSet<_>>();
        UiAction::DraftAttachments {
            draft_id: self.draft.draft_id.clone(),
            revision: self.draft.revision,
            attachment_ids: attachment_ids.into_iter().collect(),
        }
    }
    pub(super) fn finish_saved_input(
        &mut self,
        draft_id: String,
        revision: u64,
        thread_id: String,
        status: ReceiptStatus,
    ) {
        // Durable operation-correlated commit evidence outranks an earlier
        // local observation failure; it must never leave the same body unsent.
        if let Some(pending) = self
            .pending
            .get_mut(&revision)
            .filter(|p| p.snapshot.draft_id == draft_id && p.send.is_some())
        {
            pending.status = ReceiptStatus::Accepting;
        }
        self.on_receipt(draft_id, revision, Some(thread_id), status, None);
        self.pending.remove(&revision);
    }
    pub(super) fn reconcile_saved_inputs(&mut self) {
        let saved = self
            .pending
            .iter()
            .filter(|(_, p)| {
                p.send.is_some()
                    && (p.operation_id.as_ref().is_some_and(|operation| {
                        self.pending_messages
                            .items
                            .iter()
                            .any(|m| &m.operation_id == operation)
                    }) || self.pending_messages.submitted.iter().any(|m| {
                        m.source_draft_id == p.snapshot.draft_id
                            && m.revision == p.snapshot.revision
                            && p.operation_id
                                .as_ref()
                                .is_none_or(|operation| operation == &m.operation_id)
                    }) || p.source_node_id.as_ref().is_some_and(|id| {
                        self.pending_messages
                            .items
                            .iter()
                            .any(|m| &m.message_id == id)
                            || self.nodes.iter().any(|n| &n.node_id == id)
                    }))
            })
            .map(|(revision, p)| (*revision, p.snapshot.draft_id.clone()))
            .collect::<Vec<_>>();
        for (revision, draft_id) in saved {
            if let Some(thread) = self.thread_id.clone() {
                self.finish_saved_input(draft_id, revision, thread, ReceiptStatus::Accepted);
            }
        }
    }
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
            !(matches!(
                pending.status,
                ReceiptStatus::Completed | ReceiptStatus::Applied
            ) && pending.source_node_id.is_some())
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
            if stage.is_none()
                || (stage != Some(activity::ActivityPhase::Sending)
                    && phase == activity::ActivityPhase::Sending)
            {
                stage = Some(phase);
                started = Some(send.started_at);
            } else if stage == Some(phase) {
                started =
                    Some(started.map_or(send.started_at, |at: Instant| at.min(send.started_at)));
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
        self.navigation.on_event(NavEvent::Cancel {
            keep_pending: false,
        });
    }

    pub(super) fn clear_tool_observation(&mut self) -> Option<UiAction> {
        let existed = self.tool_observation_scope.take().is_some();
        self.tool_refresh = None;
        self.tool_refresh_dirty = false;
        self.tool_observation_checked = None;
        existed.then(|| UiAction::ClearToolObservation {
            thread_id: self.thread_id.clone().expect("observed view has Thread"),
            generation: self.generation,
        })
    }
    pub(super) fn request_tool_refresh(&mut self) -> Option<UiAction> {
        if self.navigation.follow() {
            return self.clear_tool_observation();
        }
        let thread_id = self.thread_id.clone()?;
        let checked = (self.navigation.view_revision, self.history_revision);
        if !self.tool_refresh_dirty && self.tool_observation_checked == Some(checked) {
            return None;
        }
        self.tool_observation_checked = Some(checked);
        let focus = projection_node_index(self);
        let start = focus.saturating_sub(64);
        let end = (start + 128).min(self.nodes.len());
        let ids = self.nodes[start..end]
            .iter()
            .filter(|node| {
                node.presentation
                    .fields
                    .iter()
                    .any(|f| f.key == "/ui_notice/title")
                    || node
                        .presentation
                        .blocks
                        .iter()
                        .any(|block| block.tool.is_some())
            })
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>();
        let scope = (self.navigation.view_revision, ids);
        let changed = self.tool_observation_scope.as_ref() != Some(&scope);
        if self.tool_refresh.is_some() {
            self.tool_refresh_dirty |= changed;
            return None;
        }
        if !changed && !self.tool_refresh_dirty {
            return None;
        }
        self.tool_refresh_dirty = false;
        if scope.1.is_empty() {
            return self.clear_tool_observation();
        }
        let nodes = self.nodes[start..end]
            .iter()
            .filter_map(UiNode::tool_stub)
            .collect();
        self.tool_observation_clock = self.tool_observation_clock.wrapping_add(1);
        let revision = self.tool_observation_clock;
        self.tool_observation_scope = Some(scope);
        self.tool_refresh = Some(revision);
        Some(UiAction::RefreshTools {
            thread_id,
            generation: self.generation,
            revision,
            view_revision: self.navigation.view_revision,
            nodes,
        })
    }
    pub(super) fn apply_tool_observation(
        &mut self,
        observation: ToolObservation,
        read_revision: u64,
    ) {
        if self.tool_refresh == Some(observation.revision) {
            self.tool_refresh = None;
        }
        if observation.revision != self.tool_observation_clock {
            return;
        }
        let mut changed = false;
        if !self.navigation.follow() && observation.view_revision == self.navigation.view_revision {
            for node in observation.nodes {
                if let Some(existing) = self
                    .nodes
                    .iter_mut()
                    .find(|old| old.node_id == node.node_id)
                    && read_revision >= existing.presentation.read_revision
                {
                    changed |= existing.refresh_tools(&node);
                    existing.presentation.read_revision = read_revision;
                }
            }
        }
        if changed {
            self.mark_source_changed();
        }
    }
    pub(super) fn start_rebase_navigation(&mut self) {
        self.tool_refresh = None;
        self.tool_refresh_dirty = false;
        self.navigation.on_event(NavEvent::StartRebase);
    }
    pub(super) fn prepare_navigation(&mut self) {
        self.tool_refresh = None;
        self.tool_refresh_dirty = false;
    }
    pub(super) fn follow_tail(&mut self) {
        self.tool_refresh = None;
        self.tool_refresh_dirty = false;
        self.navigation.on_event(NavEvent::TailRequested);
    }
    pub(super) fn new(config: &Config, cwd: PathBuf) -> Self {
        let mut live_work = super::live_work::LiveWorkView::default();
        live_work.set_language(config.ui.language);
        Self {
            language: config.ui.language,
            palette: theme::Palette::from_capabilities(config.ui.palette, false, true),
            describe_calls: config.shell.describe_calls,
            items: Default::default(),
            history_focus_item: None,
            operation_notice: None,
            agents_path: cwd.join("AGENTS.md").to_string_lossy().into_owned(),
            selecting_agents: None,
            activity: Default::default(),
            live_work,
            held_input: Default::default(),
            thread_id: None,
            cwd,
            connection: config.default_connection.clone(),
            model: config.default_model.clone(),
            effort: config
                .default_connection
                .as_ref()
                .and_then(|name| config.connections.get(name))
                .and_then(|c| c.default_effort.clone()),
            auto_compact_at: None,
            title: super::i18n::local!(config.ui.language, "新しい会話", "New conversation").into(),
            state: "idle".into(),
            draft: DraftSnapshot::default(),
            generation: 0,
            nodes: Vec::new(),
            provisional_nodes: BTreeSet::new(),
            committed_nodes: BTreeMap::new(),
            before_cursor: None,
            first_ordinal: 0,
            selection_revision: 0,
            received_revision: 0,
            received_window_revision: None,
            total_nodes: 0,
            status: crate::ui::i18n::local!(config.ui.language, "入力できます", "Ready for input")
                .into(),
            pending: BTreeMap::new(),
            pending_messages: PendingMessages::default(),
            delivery_read_revision: 0,
            tool_refresh: None,
            tool_refresh_dirty: false,
            tool_observation_scope: None,
            tool_observation_clock: 0,
            tool_observation_checked: None,
            save_retry: false,
            draft_restore_pending: false,
            run_id: None,
            turn_id: None,
            context_usage: Default::default(),
            tool_calls: 0,
            cancelled_calls: 0,
            counts_capped: false,
            reasoning_expanded: BTreeSet::new(),
            history_revision: 0,
            body_storage_bytes: std::cell::Cell::new(None),
            navigation: Navigation::default(),
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
        if self.navigation.scroll.anchor != self.navigation.anchor || self.navigation.follow() {
            self.navigation.scroll.focus = None;
        }
        if self
            .navigation
            .scroll
            .focus
            .as_ref()
            .is_none_or(|id| !self.nodes.iter().any(|n| &n.node_id == id))
        {
            let focus = self
                .nodes
                .get(viewport_node_index(self))
                .map(|node| node.node_id.clone());
            self.navigation.scroll.focus = focus;
        }
        cache.evict(budget);
        let key = HistoryLayoutKey::new(view_key, self, width, height);
        cache.discard_old_revision(&key);
        if let Some(value) = cache.get(&key) {
            return value;
        }
        let (styled, targets, sources) =
            presentation::history_with_cache(self, width, Some(cache), budget);
        let text = presentation::plain(&styled);
        // history_with_cache already returns physical hanging-wrapped rows.
        let lines = if width == 0 { 0 } else { styled.len() };
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

    // A typed, closed result's rendered rows do not depend on its hidden output.
    // Mixed/prose sources and expanded/error previews still need body windows.
    pub(super) fn closed_tool_source(&self, node: &UiNode) -> bool {
        node.presentation.kind == crate::presentation::PresentationKind::ToolResult
            && !node.presentation.blocks.is_empty()
            && node
                .presentation
                .blocks
                .iter()
                .enumerate()
                .all(|(index, block)| {
                    block.tool.as_ref().is_some_and(|tool| {
                        !self.items.is_open(
                            &ItemKey::tool(tool, &node.node_id, index),
                            tool.requires_attention(),
                            &presentation::tool_outcome(tool),
                        )
                    })
                })
    }

    fn selected_node_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|node| !self.provisional_nodes.contains(&node.node_id))
            .count()
    }
    /// Join only proven, contiguous portions of the same selected path. A body-only
    /// reply has no ordinal and therefore must never extend this ordered cache.
    pub(super) fn merge_navigation_window(&mut self, incoming: &mut Vec<UiNode>, first: &mut u64) {
        let old_start = self.first_ordinal;
        let selected = self
            .nodes
            .iter()
            .filter(|node| !self.provisional_nodes.contains(&node.node_id))
            .collect::<Vec<_>>();
        let old_end = old_start + selected.len() as u64;
        let new_end = *first + incoming.len() as u64;
        let overlap_start = old_start.max(*first);
        let overlap_end = old_end.min(new_end);
        if overlap_start >= overlap_end
            || (overlap_start..overlap_end).any(|ordinal| {
                selected[(ordinal - old_start) as usize].node_id
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
        let mut ordinal = old_start;
        for mut node in std::mem::take(&mut self.nodes) {
            if self.provisional_nodes.contains(&node.node_id) {
                self.nodes.push(node);
                continue;
            }
            if let Some(loaded) = ordered.get_mut(&ordinal) {
                if !node.presentation.body_evicted
                    && !node.text.is_empty()
                    && (loaded.presentation.body_evicted
                        || node.presentation.read_revision >= loaded.presentation.read_revision)
                {
                    let total = node.total_chars.max(loaded.total_chars);
                    *loaded = node;
                    loaded.total_chars = total;
                } else {
                    loaded.preserve_preview_on_transient_failure(&mut node);
                }
            } else {
                ordered.insert(ordinal, node);
            }
            ordinal += 1;
        }
        *first = old_start.min(*first);
        *incoming = ordered.into_values().collect();
    }
    pub(super) fn viewport_node_index(&self) -> usize {
        if self.navigation.follow() {
            return self.nodes.len().saturating_sub(1);
        }
        self.navigation
            .anchor
            .node_id
            .as_ref()
            .and_then(|id| self.nodes.iter().position(|node| &node.node_id == id))
            .unwrap_or(0)
    }
    pub(super) fn projection_node_index(&self) -> usize {
        self.navigation
            .scroll
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
        self.agents_path = details.agents_path;
        self.cwd = PathBuf::from(details.cwd);
        self.connection = Some(details.connection);
        self.model = Some(details.model);
        self.effort = details.effort;
        self.auto_compact_at = details.auto_compact_at;
        self.title = details.title;
        self.run_id = details.run_id;
        self.turn_id = details.turn_id;
        self.state = if details.recovery_reason.is_some() {
            crate::ui::i18n::local!(self.language, "復旧待ち", "Waiting for recovery").into()
        } else if self.run_id.is_some() {
            crate::ui::i18n::local!(self.language, "処理中", "Running").into()
        } else {
            crate::ui::i18n::local!(self.language, "待機中", "Idle").into()
        };
        if let Some(reason) = details.recovery_reason {
            self.status = match serde_json::from_str::<crate::protocol::ApiError>(&reason) {
                Ok(error) => {
                    let message = crate::config_setup::tui_error(&error);
                    match self.language {
                        crate::config::UiLanguage::Ja => format!("復旧待ち: {message}"),
                        crate::config::UiLanguage::En => format!("Recovery pending: {message}"),
                    }
                }
                Err(_) => super::i18n::local!(
                    self.language,
                    "復旧待ち: 保存された復旧理由を詳細で確認してください",
                    "Recovery pending: Check the saved recovery reason in details"
                )
                .into(),
            };
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
        mut nodes: Vec<UiNode>,
        before_cursor: Option<String>,
        append: bool,
        first_ordinal: u64,
        selection_revision: u64,
        total_nodes: u64,
    ) -> bool {
        if selection_revision < self.selection_revision {
            return false;
        }
        for incoming in &mut nodes {
            if let Some(existing) = self
                .nodes
                .iter_mut()
                .find(|old| old.node_id == incoming.node_id)
            {
                if !existing.presentation.body_evicted
                    && (incoming.presentation.body_evicted
                        || incoming.presentation.read_revision
                            < existing.presentation.read_revision)
                {
                    *incoming = existing.clone();
                } else if existing.same_projection(incoming) {
                    existing.presentation.read_revision = existing
                        .presentation
                        .read_revision
                        .max(incoming.presentation.read_revision);
                }
            }
        }
        let mut source_changed = false;
        if self.navigation.follow() && !self.navigation.is_rebasing() {
            source_changed |= self.retire_committed_preview(&nodes);
        }
        let previous_tail = self.nodes.last().map(|node| node.node_id.clone());
        if self.navigation.is_rebasing() {
            // A tail refresh cannot replace the frame whose source
            // anchor is being rebound to the selected path.
            self.navigation.set_unseen(true);
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
        } else if !self.navigation.follow() && !self.nodes.is_empty() {
            let incoming_tail = nodes.last().map(|node| node.node_id.as_str());
            if incoming_tail != previous_tail.as_deref() {
                self.navigation.set_unseen(true);
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
                    if self.navigation.anchor.node_id.as_deref() == Some(&node.node_id) {
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
            let received_bindings = self.received_anchor_bindings();
            let received = self.take_received_previews(&received_bindings);
            let mut nodes = nodes;
            for incoming in &mut nodes {
                if let Some(existing) = self
                    .nodes
                    .iter_mut()
                    .find(|old| old.node_id == incoming.node_id)
                {
                    incoming.preserve_preview_on_transient_failure(existing);
                }
            }
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
            self.on_incomplete_responses(received);
            self.first_ordinal = first_ordinal;
            self.selection_revision = selection_revision;
        }
        if source_changed {
            self.mark_source_changed();
        }
        self.observe_sent_nodes();
        self.total_nodes = total_nodes;
        if append || self.navigation.follow() || previous_tail.is_none() {
            self.before_cursor = before_cursor;
        }
        self.navigation
            .on_event(NavEvent::Read(ReadEvent::BodyHydrated));
        if !self.navigation.follow()
            && !append
            && previous_tail.is_some()
            && previous_tail.as_deref() != self.nodes.last().map(|node| node.node_id.as_str())
        {
            self.navigation.set_unseen(true);
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
        let previous_total = self.body_storage_bytes.get();
        let previous_cost = self
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .map_or(0, node_storage_bytes);
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
                && (node.presentation.projection_stamp.is_some()
                    && node.presentation.projection_stamp == presentation.projection_stamp
                    || node.presentation.projection_stamp.is_none()
                        && presentation.projection_stamp.is_none()
                        && node.text == text
                        && node.presentation == presentation)
            {
                return;
            }
            source_changed = true;
            if self.navigation.follow()
                || self.navigation.anchor.node_id.as_deref() != Some(&node_id)
                || (self.navigation.anchor.char_offset >= base_char_offset
                    && self.navigation.anchor.char_offset < base_char_offset + text.chars().count())
            {
                node.text = text;
                node.presentation = presentation;
                node.base_char_offset = base_char_offset;
                node.reasoning_ranges = reasoning_ranges;
            }
            node.total_chars = total_chars;
        } else if self.navigation.follow() {
            source_changed = true;
            self.provisional_nodes.insert(node_id.clone());
            self.nodes.push(UiNode {
                presentation,
                node_id: node_id.clone(),
                role,
                text,
                base_char_offset,
                total_chars,
                reasoning_ranges,
            });
        }
        if source_changed {
            let updated_cost = self
                .nodes
                .iter()
                .find(|node| node.node_id == node_id)
                .map_or(0, node_storage_bytes);
            self.mark_source_changed();
            self.body_storage_bytes.set(previous_total.map(|total| {
                total
                    .saturating_sub(previous_cost)
                    .saturating_add(updated_cost)
            }));
        }
        if !self.navigation.follow() {
            self.navigation.set_unseen(true);
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
        // Paused and following previews share the same durable-source binding.
        // Rebase must not discard or duplicate the preview before hydration.
        self.committed_nodes
            .insert(attempt_id.clone(), node_id.clone());
        if !self.navigation.follow()
            && self.navigation.anchor.node_id.as_deref() == Some(&attempt_id)
        {
            self.navigation.committed_preview = Some((attempt_id, node_id));
            return self.rebase_preview();
        }
        if !self.navigation.follow() {
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
                    &node.node_id == *saved
                        && !node.presentation.body_evicted
                        && (node.total_chars == 0 || !node.text.is_empty())
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
                    &node.node_id == *saved
                        && !node.presentation.body_evicted
                        && (node.total_chars == 0 || !node.text.is_empty())
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
                .navigation
                .committed_preview
                .as_ref()
                .is_some_and(|(id, _)| id == attempt)
            {
                self.navigation.committed_preview = None;
            }
        }
        !retired.is_empty()
    }
    fn rebase_preview(&mut self) -> Option<UiAction> {
        if self.navigation.is_rebasing() {
            return None;
        }
        let (attempt, node_id) = self.navigation.committed_preview.clone()?;
        let Some(node) = self.nodes.iter().find(|node| node.node_id == attempt) else {
            self.navigation.committed_preview = None;
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
        self.start_rebase_navigation();
        Some(UiAction::RebaseStream(Box::new(StreamRebase {
            thread_id: self.thread_id.clone()?,
            generation: self.generation,
            view_revision: self.navigation.view_revision,
            node_id,
            source,
            anchor: self.navigation.anchor.clone(),
        })))
    }
    pub(super) fn rebase_committed_stream(
        &mut self,
        thread_id: String,
        generation: u64,
    ) -> Option<UiAction> {
        if !self.navigation.follow()
            && !self.navigation.is_rebasing()
            && !self
                .navigation
                .anchor
                .node_id
                .as_ref()
                .is_some_and(|id| self.provisional_nodes.contains(id))
        {
            // Keep this frame while resolving real ordinals and source
            // offsets. No provisional ID is sent to history.get.
            self.start_rebase_navigation();
            let action = UiAction::RebaseHistory {
                thread_id,
                generation,
                view_revision: self.navigation.view_revision,
                anchor: self.navigation.anchor.clone(),
            };
            return Some(action);
        }
        None
    }
    pub(super) fn on_view_state(&mut self, anchor: ViewAnchor, follow: bool) {
        self.navigation
            .on_event(NavEvent::Restored { anchor, follow });
    }
    pub(super) fn on_load_failed(&mut self, message: String) {
        self.navigation.on_event(NavEvent::Read(ReadEvent::Failed));
        self.status = message;
        self.promote_status_notice(history_interaction::NoticeKind::Failure);
    }
    pub(super) fn on_node_window(&mut self, view_revision: u64, mut node: UiNode) {
        let node_id_for_reply = node.node_id.clone();
        let body_ready =
            !node.presentation.body_evicted && (node.total_chars == 0 || !node.text.is_empty());
        let mut installed = false;
        if let Some(existing) = self
            .nodes
            .iter_mut()
            .find(|existing| existing.node_id == node.node_id)
        {
            if node.presentation.read_revision < existing.presentation.read_revision {
                return;
            }
            installed = true;
            let accepted_read = existing
                .presentation
                .read_revision
                .max(node.presentation.read_revision);
            if !existing.same_projection(&node) || existing.presentation.body_evicted {
                node.preserve_preview_on_transient_failure(existing);
                node.presentation.read_revision = accepted_read;
                *existing = node;
                self.mark_source_changed();
            } else {
                existing.presentation.read_revision = accepted_read;
            }
        }
        // A source-window response does not install an off-page node. Its slot
        // may have been evicted while this asynchronous read was pending.
        if installed && body_ready && self.navigation.follow() && !self.navigation.is_rebasing() {
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
        if self.navigation.request().is_none()
            || self.navigation.request() == Some(&(view_revision, Some(node_id_for_reply.clone())))
        {
            self.navigation.set_loading(false);
        }
    }
    fn received_anchor_bindings(&self) -> Vec<(String, String)> {
        let mut predecessor: Option<String> = None;
        let mut bindings = Vec::new();
        for node in &self.nodes {
            if node.role == "received_response" {
                if let Some(predecessor) = &predecessor {
                    bindings.push((node.node_id.clone(), predecessor.clone()));
                }
            } else if !self.provisional_nodes.contains(&node.node_id) {
                predecessor = Some(node.node_id.clone());
            }
        }
        bindings
    }
    fn take_received_previews(&mut self, bindings: &[(String, String)]) -> Vec<(String, UiNode)> {
        let mut received = Vec::new();
        let mut removed = false;
        for node in std::mem::take(&mut self.nodes) {
            if node.role == "received_response" {
                removed = true;
                if let Some((_, predecessor)) = bindings.iter().find(|(id, _)| id == &node.node_id)
                {
                    received.push((predecessor.clone(), node));
                }
            } else {
                self.nodes.push(node);
            }
        }
        if removed {
            self.mark_source_changed();
        }
        received
    }
    /// Returns true when a non-tail snapshot requires the ordinary latest page.
    pub(super) fn on_history_window(
        &mut self,
        mut nodes: Vec<UiNode>,
        mut first_ordinal: u64,
        selection_revision: u64,
        total_nodes: u64,
        anchor: ViewAnchor,
        window_reply: bool,
    ) -> bool {
        if selection_revision < self.selection_revision {
            return false;
        }
        let following = self.navigation.follow();
        let received_bindings = self.received_anchor_bindings();
        if following
            && first_ordinal.saturating_add(nodes.len() as u64) < total_nodes
            && self
                .first_ordinal
                .saturating_add(self.selected_node_count() as u64)
                < total_nodes
        {
            self.navigation
                .on_event(NavEvent::Read(ReadEvent::RebaseResolved));
            self.navigation.set_loading(false);
            return true;
        }
        if (following || (window_reply && !self.navigation.is_rebasing()))
            && selection_revision == self.selection_revision
        {
            merge_navigation_window(self, &mut nodes, &mut first_ordinal);
        }
        if following {
            // An anchor window is not necessarily the latest page. Preserve the
            // paid/current frame until a real tail snapshot arrives.
            if first_ordinal.saturating_add(nodes.len() as u64) < total_nodes {
                self.navigation
                    .on_event(NavEvent::Read(ReadEvent::RebaseResolved));
                self.navigation.set_loading(false);
                return true;
            }
            self.navigation
                .on_event(NavEvent::Read(ReadEvent::RebaseResolved));
            // A history window is not an authoritative audit snapshot. Keep
            // paid bodies at their surviving selected predecessors until that
            // separately fenced audit reply decides membership.
            let received = self.take_received_previews(&received_bindings);
            self.on_history(
                nodes,
                None,
                false,
                first_ordinal,
                selection_revision,
                total_nodes,
            );
            self.on_incomplete_responses(received);
            return false;
        }
        let received = self.take_received_previews(&received_bindings);
        // Rebase is an ordinal update, not proof that a committed stream's
        // canonical body was fetched. Retire only verified loaded sources.
        self.retire_committed_preview(&nodes);
        let previews = self
            .nodes
            .drain(..)
            .filter(|node| {
                self.provisional_nodes.contains(&node.node_id)
                    && node.role != "received_response"
                    && !nodes.iter().any(|saved| saved.node_id == node.node_id)
            })
            .collect::<Vec<_>>();
        nodes.extend(previews);
        self.mark_source_changed();
        self.nodes = nodes;
        self.provisional_nodes
            .retain(|id| self.nodes.iter().any(|node| &node.node_id == id));
        self.on_incomplete_responses(received);
        let relocated = self.navigation.is_rebasing()
            && !self
                .nodes
                .iter()
                .any(|node| Some(&node.node_id) == self.navigation.anchor.node_id.as_ref());
        self.observe_sent_nodes();
        self.first_ordinal = first_ordinal;
        self.selection_revision = selection_revision;
        self.total_nodes = total_nodes;
        if !window_reply
            || (self.navigation.is_rebasing()
                && !self.navigation.moved_during_read()
                && self.navigation.anchor != anchor)
            || !self
                .nodes
                .iter()
                .any(|node| Some(&node.node_id) == self.navigation.anchor.node_id.as_ref())
        {
            self.navigation.anchor = anchor;
            self.navigation.scroll.within = 0;
        }
        self.before_cursor = None;
        self.navigation.set_loading(false);
        self.navigation
            .on_event(NavEvent::Read(ReadEvent::PositionSettled));
        if self.navigation.is_rebasing() {
            self.navigation
                .on_event(NavEvent::Read(ReadEvent::RebaseResolved));
            self.status = if relocated {
                super::i18n::local!(
                    self.language,
                    "表示元が選択経路から外れたため、最新ページを表示しました",
                    "The display source is no longer on the selected path. Showing the latest page"
                )
            } else {
                super::i18n::local!(
                    self.language,
                    "履歴の表示位置を再取得しました",
                    "Restored the history display position"
                )
            }
            .into();
        }
        false
    }
    pub(super) fn on_history_invalidated(
        &mut self,
        thread_id: String,
        generation: u64,
        view_revision: Option<u64>,
    ) -> Option<UiAction> {
        if self.thread_id.as_deref() != Some(&thread_id)
            || self.generation != generation
            || view_revision.is_some_and(|revision| revision != self.navigation.view_revision)
            || (view_revision.is_none() && self.navigation.follow())
            || self.navigation.is_rebasing()
        {
            return None;
        }
        self.start_rebase_navigation();
        self.status = crate::ui::text_width::glyphs::localized!(
            self.language,
            "履歴の表示位置を再取得中…",
            "履歴の表示位置を再取得中.",
            "Restoring history position…",
            "Restoring history position."
        )
        .into();
        let action = UiAction::RebaseHistory {
            thread_id,
            generation,
            view_revision: self.navigation.view_revision,
            anchor: self.navigation.anchor.clone(),
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
                node.presentation.projection_stamp = None;
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

// Keep each combination as a complete message rather than joining translated fragments.
fn receipt_english(
    status: &ReceiptStatus,
    revision: u64,
    message: Option<&str>,
    retained: bool,
) -> String {
    match (status, message, retained) {
        (ReceiptStatus::Accepting, None, false) => "Accepting".into(),
        (ReceiptStatus::Accepting, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Accepting · Current input retained (next send uses the full text)",
            "Accepting | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Accepting, Some(message), false) => format!("Accepting: {message}"),
        (ReceiptStatus::Accepting, Some(message), true) => {
            crate::ui::text_width::glyphs::formatted!(
                "Accepting: {message} · Current input retained (next send uses the full text)",
                "Accepting: {message} | Current input retained (next send uses the full text)"
            )
        }
        (ReceiptStatus::Accepted, None, false) => "Accepted".into(),
        (ReceiptStatus::Accepted, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Accepted · Current input retained (next send uses the full text)",
            "Accepted | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Accepted, Some(message), false) => format!("Accepted: {message}"),
        (ReceiptStatus::Accepted, Some(message), true) => {
            crate::ui::text_width::glyphs::formatted!(
                "Accepted: {message} · Current input retained (next send uses the full text)",
                "Accepted: {message} | Current input retained (next send uses the full text)"
            )
        }
        (ReceiptStatus::Preparing, None, false) => "Accepted / Preparing".into(),
        (ReceiptStatus::Preparing, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Accepted / Preparing · Current input retained (next send uses the full text)",
            "Accepted / Preparing | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Preparing, Some(message), false) => {
            format!("Accepted / Preparing: {message}")
        }
        (ReceiptStatus::Preparing, Some(message), true) => {
            crate::ui::text_width::glyphs::formatted!(
                "Accepted / Preparing: {message} · Current input retained (next send uses the full text)",
                "Accepted / Preparing: {message} | Current input retained (next send uses the full text)"
            )
        }
        (ReceiptStatus::Held, None, false) => "Held".into(),
        (ReceiptStatus::Held, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Held · Current input retained (next send uses the full text)",
            "Held | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Held, Some(message), false) => format!("Held: {message}"),
        (ReceiptStatus::Held, Some(message), true) => crate::ui::text_width::glyphs::formatted!(
            "Held: {message} · Current input retained (next send uses the full text)",
            "Held: {message} | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Applied, None, false) => "Applied to conversation".into(),
        (ReceiptStatus::Applied, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Applied to conversation · Current input retained (next send uses the full text)",
            "Applied to conversation | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Applied, Some(message), false) => {
            format!("Applied to conversation: {message}")
        }
        (ReceiptStatus::Applied, Some(message), true) => crate::ui::text_width::glyphs::formatted!(
            "Applied to conversation: {message} · Current input retained (next send uses the full text)",
            "Applied to conversation: {message} | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Failed, None, false) => crate::ui::text_width::glyphs::formatted!(
            "Failed · /restore {revision} to restore the original input",
            "Failed | /restore {revision} to restore the original input"
        ),
        (ReceiptStatus::Failed, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Failed · /restore {revision} to restore the original input · Current input retained (next send uses the full text)",
            "Failed | /restore {revision} to restore the original input | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Failed, Some(message), false) => crate::ui::text_width::glyphs::formatted!(
            "Failed · /restore {revision} to restore the original input: {message}",
            "Failed | /restore {revision} to restore the original input: {message}"
        ),
        (ReceiptStatus::Failed, Some(message), true) => crate::ui::text_width::glyphs::formatted!(
            "Failed · /restore {revision} to restore the original input: {message} · Current input retained (next send uses the full text)",
            "Failed | /restore {revision} to restore the original input: {message} | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Cancelled, None, false) => crate::ui::text_width::glyphs::formatted!(
            "Cancelled · /restore {revision} to restore the original input",
            "Cancelled | /restore {revision} to restore the original input"
        ),
        (ReceiptStatus::Cancelled, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Cancelled · /restore {revision} to restore the original input · Current input retained (next send uses the full text)",
            "Cancelled | /restore {revision} to restore the original input | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Cancelled, Some(message), false) => {
            crate::ui::text_width::glyphs::formatted!(
                "Cancelled · /restore {revision} to restore the original input: {message}",
                "Cancelled | /restore {revision} to restore the original input: {message}"
            )
        }
        (ReceiptStatus::Cancelled, Some(message), true) => {
            crate::ui::text_width::glyphs::formatted!(
                "Cancelled · /restore {revision} to restore the original input: {message} · Current input retained (next send uses the full text)",
                "Cancelled | /restore {revision} to restore the original input: {message} | Current input retained (next send uses the full text)"
            )
        }
        (ReceiptStatus::Completed, None, false) => "Completed".into(),
        (ReceiptStatus::Completed, None, true) => crate::ui::text_width::glyphs::formatted!(
            "Completed · Current input retained (next send uses the full text)",
            "Completed | Current input retained (next send uses the full text)"
        ),
        (ReceiptStatus::Completed, Some(message), false) => format!("Completed: {message}"),
        (ReceiptStatus::Completed, Some(message), true) => {
            crate::ui::text_width::glyphs::formatted!(
                "Completed: {message} · Current input retained (next send uses the full text)",
                "Completed: {message} | Current input retained (next send uses the full text)"
            )
        }
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
            ReceiptStatus::Failed
                | ReceiptStatus::Cancelled
                | ReceiptStatus::Completed
                | ReceiptStatus::Applied
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
        if pending.status == ReceiptStatus::Accepting
            && matches!(
                status,
                ReceiptStatus::Accepted | ReceiptStatus::Preparing | ReceiptStatus::Completed
            )
            && let Some(send) = &mut pending.send
        {
            send.started_at = Instant::now();
        }
        if pending.send.is_some()
            && pending.status == ReceiptStatus::Accepting
            && pending.source_node_id.is_none()
            && matches!(status, ReceiptStatus::Failed | ReceiptStatus::Cancelled)
            && pending.cleared_draft.as_ref() == Some(&self.draft)
        {
            let next = self.draft.revision + 1;
            self.draft = pending.snapshot.clone();
            self.draft.revision = next;
            pending.restored_revision = Some(next);
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
                | ReceiptStatus::Applied
                | ReceiptStatus::Completed
        );
        if accepted
            && self.draft.draft_id == draft_id
            && (self.draft.revision == revision
                || pending.restored_revision == Some(self.draft.revision))
        {
            self.draft.text = String::new();
            self.draft.attachments = Vec::new();
            self.draft.folds = Vec::new();
            self.draft.cursor = 0;
            self.draft.literal_paste = false;
            self.draft.typed_prefix = 0;
            self.draft.edit = None;
            self.draft.revision += 1;
        }
        let retained = accepted
            && self.draft.draft_id == draft_id
            && self.draft.revision != revision
            && (!self.draft.text.is_empty() || !self.draft.attachments.is_empty());
        if self.language == crate::config::UiLanguage::En {
            self.status = receipt_english(&status, revision, message.as_deref(), retained);
        } else {
            self.status = format!(
                "{}{}",
                if matches!(status, ReceiptStatus::Failed | ReceiptStatus::Cancelled) {
                    crate::ui::text_width::glyphs::formatted!(
                        "{} · /restore {revision} で元の入力を復元",
                        "{} | /restore {revision} で元の入力を復元",
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
                self.status.push_str(crate::ui::text_width::glyphs::select(
                    " · 編集中の入力を保持（次は全文送信）",
                    " | 編集中の入力を保持（次は全文送信）",
                ));
            }
        }
        if matches!(status, ReceiptStatus::Completed | ReceiptStatus::Applied)
            && (pending.send.is_none() || pending.source_node_id.is_some())
        {
            self.pending.remove(&revision);
        }
        if status == ReceiptStatus::Failed {
            self.promote_status_notice(history_interaction::NoticeKind::Failure);
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
        draft.typed_prefix = 0;
        self.draft = draft;
        self.status = super::i18n::local!(
            self.language,
            "保存済みの依頼を入力欄へ戻しました",
            "Restored the saved request to the input field"
        )
        .into();
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
            self.status = super::i18n::local!(
                self.language,
                "画像位置または下書きのbyte上限が変わったため採用していません",
                "The image was not attached because the image position or draft byte limit changed"
            )
            .into();
            false
        } else {
            self.draft.insert_attachment(attachment, position);
            self.draft.revision += 1;
            self.status = match self.language {
                crate::config::UiLanguage::Ja => "画像を添付しました",
                crate::config::UiLanguage::En => "Image attached",
            }
            .into();
            true
        }
    }
    pub(super) fn on_progress(
        &mut self,
        tool_calls: u64,
        cancelled_calls: u64,
        counts_capped: bool,
        state: String,
    ) {
        self.tool_calls = tool_calls;
        self.cancelled_calls = cancelled_calls;
        self.counts_capped = counts_capped;
        self.state = state;
    }
}

impl ThreadView {
    pub(super) fn retain_source_ids(&mut self) {
        let ids = self
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<BTreeSet<_>>();
        self.provisional_nodes
            .retain(|id| ids.contains(id.as_str()));
        self.committed_nodes
            .retain(|attempt, _| self.provisional_nodes.contains(attempt));
        if self
            .navigation
            .committed_preview
            .as_ref()
            .is_some_and(|(id, _)| !self.provisional_nodes.contains(id))
        {
            self.navigation.committed_preview = None;
        }
        self.reasoning_expanded
            .retain(|id| ids.contains(id.as_str()));
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
    pub(super) fn current_uncommitted_preview(&self) -> Option<String> {
        self.nodes
            .iter()
            .rev()
            .find(|node| {
                self.provisional_nodes.contains(&node.node_id)
                    && !self.committed_nodes.contains_key(&node.node_id)
                    && node.role != "received_response"
            })
            .map(|node| node.node_id.clone())
    }
    /// Committed preview IDs name attempts, not history nodes. An offscreen
    /// preview can be dropped as a unit once its canonical source is saved;
    /// retaining an empty attempt-ID stub would make later hydration impossible.
    pub(super) fn discard_committed_previews_except(&mut self, protected: &[String]) -> usize {
        let before = self.retained_body_bytes(protected);
        let retired = self
            .committed_nodes
            .keys()
            .filter(|id| !protected.contains(id))
            .cloned()
            .collect::<Vec<_>>();
        if self.remove_committed_previews(retired) {
            self.mark_source_changed();
        }
        before.saturating_sub(self.retained_body_bytes(protected))
    }
    pub(super) fn discard_cached_history(&mut self) {
        self.nodes = Vec::new();
        self.committed_nodes.clear();
        self.provisional_nodes.clear();
        self.navigation.committed_preview = None;
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
            if working.contains(&self.nodes[0].node_id)
                && working.contains(&self.nodes[last].node_id)
            {
                break;
            }
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
            let selected = !self.provisional_nodes.remove(&removed.node_id);
            if let Some((attempt, saved)) = self.committed_nodes.remove_entry(&removed.node_id) {
                *retained = retained
                    .saturating_sub(expanded_id_bytes(&attempt) + expanded_id_bytes(&saved));
            }
            *retained = retained.saturating_sub(removed.metadata_bytes() + removed.cached_bytes());
            if let Some(id) = self.reasoning_expanded.take(&removed.node_id) {
                *retained = retained.saturating_sub(expanded_id_bytes(&id));
            }
            if index == 0 && selected {
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
        let mut bytes = self.items.retained_bytes();
        bytes = bytes.saturating_add(
            self.nodes.capacity().saturating_sub(working.len()) * std::mem::size_of::<UiNode>(),
        );
        let stored = self.body_storage_bytes.get().unwrap_or_else(|| {
            let stored = self.nodes.iter().map(node_storage_bytes).sum();
            self.body_storage_bytes.set(Some(stored));
            stored
        });
        bytes = bytes.saturating_add(stored);
        for node in self
            .nodes
            .iter()
            .filter(|node| working.contains(&node.node_id))
        {
            bytes = bytes.saturating_sub(node_storage_bytes(node));
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
            && self.navigation.view_revision == revision
            && self
                .navigation
                .request()
                .is_some_and(|(expected, id)| *expected == revision && id.as_deref() == node)
    }
    pub(super) fn finish_navigation_reply(&mut self) -> isize {
        self.navigation.on_event(NavEvent::ReplyConsumed).pending
    }
}
impl ThreadView {
    pub(super) fn history_revision(&self) -> u64 {
        self.history_revision
    }
    fn mark_source_changed(&mut self) {
        self.body_storage_bytes.set(None);
        self.history_revision = self.history_revision.wrapping_add(1);
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    #[test]
    fn language_follows_config_for_new_views_and_separates_render_caches() {
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut state = UiState::new(&config, std::env::temp_dir());
        assert_eq!(state.view().language, crate::config::UiLanguage::Ja);
        let whole_ja = HistoryLayoutKey::new("owner", state.view(), 80, 20);
        let unit_ja = RenderUnitKey::new(state.view(), 0, 0, 80, &[]).unwrap();
        state.new_chat();
        assert_eq!(state.view().language, crate::config::UiLanguage::Ja);
        state.select_thread("saved-thread".into());
        assert_eq!(state.view().language, crate::config::UiLanguage::Ja);

        let mut english = ThreadView::new(&Config::default(), std::env::temp_dir());
        english.language = crate::config::UiLanguage::Ja;
        let whole_same = HistoryLayoutKey::new("owner", &english, 80, 20);
        let unit_same = RenderUnitKey::new(&english, 0, 0, 80, &[]).unwrap();
        english.language = crate::config::UiLanguage::En;
        assert_ne!(whole_same, HistoryLayoutKey::new("owner", &english, 80, 20));
        assert_ne!(
            unit_same,
            RenderUnitKey::new(&english, 0, 0, 80, &[]).unwrap()
        );
        assert_eq!(whole_ja.language, crate::config::UiLanguage::Ja);
        assert_eq!(unit_ja.language, crate::config::UiLanguage::Ja);
    }

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
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("source".into()),
            char_offset: 0,
        };
        let mut cache = HistoryLayoutCache::default();
        let budget = 1024 * 1024;
        let layout = view.history_layout("owner", &mut cache, budget, 35, 8);
        let mut previous = view.project_viewport(&layout, 35, 8);
        for _ in 0..30 {
            assert!(view.scroll(1, "owner", &mut cache, budget).is_empty());
            let layout = view.history_layout("owner", &mut cache, budget, 35, 8);
            let row = view.project_viewport(&layout, 35, 8);
            assert_eq!(
                row,
                previous + 1,
                "cached single-row scroll never jumps backwards"
            );
            assert_eq!(
                layout.sources[row].offset,
                view.navigation.anchor.char_offset
            );
            previous = row;
        }
        let anchor = view.navigation.anchor.clone();
        let resized = view.history_layout("owner", &mut cache, budget, 55, 8);
        let row = view.project_viewport(&resized, 55, 8);
        assert_eq!(view.navigation.anchor, anchor);
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
            .navigation
            .committed_preview
            .as_ref()
            .is_some_and(|(id, _)| id == attempt)
        {
            self.navigation.committed_preview = None;
        }
        self.nodes.retain(|node| node.node_id != attempt);
        self.mark_source_changed();
        if self.navigation.anchor.node_id.as_deref() != Some(attempt) {
            return None;
        }
        // A transient stream position is not a canonical source offset.
        self.navigation.anchor = ViewAnchor::default();
        if self.navigation.follow() {
            self.navigation.on_event(NavEvent::SourceInvalidated);
            return None;
        }
        self.start_rebase_navigation();
        Some(UiAction::RebaseHistory {
            thread_id: self.thread_id.clone()?,
            generation: self.generation,
            view_revision: self.navigation.view_revision,
            anchor: self.navigation.anchor.clone(),
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
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("attempt".into()),
            char_offset: 2,
        };
        let action = view.settle_stream("attempt").unwrap();
        assert!(view.nodes().is_empty());
        assert!(view.navigation.anchor.node_id.is_none());
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
    fn receipt_english_preserves_raw_messages_revision_and_terminal_symbols() {
        use crate::ui::text_width;
        let statuses = [
            ReceiptStatus::Accepting,
            ReceiptStatus::Accepted,
            ReceiptStatus::Preparing,
            ReceiptStatus::Held,
            ReceiptStatus::Applied,
            ReceiptStatus::Failed,
            ReceiptStatus::Cancelled,
            ReceiptStatus::Completed,
        ];
        for policy in [Some(1), Some(2), None] {
            text_width::set_terminal_width(policy);
            for status in &statuses {
                for retained in [false, true] {
                    for message in [None, Some("RAW 日本語：")] {
                        let displayed = receipt_english(status, 42, message, retained);
                        assert!(!displayed.is_empty());
                        if let Some(raw) = message {
                            assert!(displayed.contains(raw), "{displayed}");
                        }
                        if matches!(status, ReceiptStatus::Failed | ReceiptStatus::Cancelled) {
                            assert!(displayed.contains("/restore 42"), "{displayed}");
                        }
                        assert_eq!(
                            displayed.contains("Current input retained"),
                            retained,
                            "{displayed}"
                        );
                        if policy != Some(1) {
                            assert!(!displayed.contains('·'), "{displayed}");
                        }
                    }
                }
            }
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn completion_notice_requires_the_accepted_owner_revision_anchor_and_paused_view() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        let revision = ui.view().navigation.view_revision;
        ui.view_mut().navigation.select_reading();
        // Model the accepted HistoryWindow, not the initial selection load.
        ui.view_mut().navigation.set_loading(false);
        ui.view_mut().navigation.anchor = ViewAnchor {
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
        ui.view_mut().navigation.request_tail();
        ui.apply(notice(generation, revision, "saved", "stale after follow"));
        assert_eq!(ui.view().status, "verified source");
    }
    #[test]
    fn read_failure_discards_queued_motion_without_automatic_retry() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.navigation.on_event(NavEvent::ReadRequested {
            ticket: (view.navigation.view_revision, None),
            pending: None,
            new_epoch: false,
        });
        view.navigation.seed_pending(-3);
        view.on_load_failed("read permit unavailable".into());
        assert!(!view.navigation.is_loading());
        assert!(view.navigation.request().is_none());
        assert_eq!(view.navigation.pending(), 0);
        assert_eq!(view.status, "read permit unavailable");
    }
    #[test]
    fn unrelated_refresh_failure_does_not_cancel_navigation_read() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        let revision = ui.view().navigation.view_revision;
        ui.view_mut().navigation.set_loading(true);
        ui.view_mut().navigation.set_request(Some((revision, None)));
        ui.view_mut().navigation.seed_pending(-3);
        ui.apply(UiUpdate::LoadFailed {
            thread_id: "thread".into(),
            generation,
            view_revision: None,
            message: "background refresh failed".into(),
        });
        assert!(ui.view().navigation.is_loading());
        assert_eq!(ui.view().navigation.request(), Some(&(revision, None)));
        assert_eq!(ui.view().navigation.pending(), -3);
        assert_eq!(ui.view().status, "background refresh failed");
    }
    #[test]
    fn navigation_read_has_an_explicit_footer_indicator() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.navigation.on_event(NavEvent::ReadRequested {
            ticket: (view.navigation.view_revision, None),
            pending: None,
            new_epoch: false,
        });
        let palette = crate::ui::theme::Palette::from_capabilities(
            crate::config::PaletteMode::Dark,
            false,
            true,
        );
        let text = crate::ui::chrome::footer_lines(&view, 100, "", palette, vec![], None, None)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Loading history"), "{text}");
    }
    #[test]
    fn cached_rows_remain_navigable_during_a_pending_read() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        let text = (0..100)
            .map(|i| format!("cached line {i:03}\n"))
            .collect::<String>();
        let offset = "cached line 000\n".len() * 50;
        view.nodes = vec![UiNode {
            node_id: "saved".into(),
            role: "assistant".into(),
            text: text.clone(),
            total_chars: text.chars().count(),
            ..Default::default()
        }];
        view.thread_id = Some("thread".into());
        view.total_nodes = 2;
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: offset,
        };
        view.navigation.scroll.width = 80;
        view.navigation.scroll.height = 10;
        view.navigation.on_event(NavEvent::ReadRequested {
            ticket: (view.navigation.view_revision, None),
            pending: None,
            new_epoch: false,
        });
        let mut cache = HistoryLayoutCache::default();
        let actions = view.scroll(-1, "thread", &mut cache, 1024 * 1024);
        assert!(actions.is_empty());
        assert!(
            view.navigation.anchor.char_offset < offset,
            "cached rows should move during the read"
        );
        assert!(
            view.navigation.request().is_some(),
            "the original read stays owned"
        );
    }
    #[test]
    fn pending_read_allows_cached_movement_across_nodes() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.nodes = (0..30)
            .map(|i| {
                let text = format!("cached item {i:02}");
                UiNode {
                    node_id: format!("node-{i:02}"),
                    role: "assistant".into(),
                    total_chars: text.chars().count(),
                    text,
                    ..Default::default()
                }
            })
            .collect();
        view.thread_id = Some("thread".into());
        view.total_nodes = 31;
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("node-15".into()),
            char_offset: 0,
        };
        view.navigation.scroll.width = 80;
        view.navigation.scroll.height = 10;
        view.navigation.on_event(NavEvent::ReadRequested {
            ticket: (view.navigation.view_revision, None),
            pending: None,
            new_epoch: false,
        });
        let mut cache = HistoryLayoutCache::default();
        view.scroll(-2, "thread", &mut cache, 1024 * 1024);
        assert_ne!(view.navigation.anchor.node_id.as_deref(), Some("node-15"));
        assert!(view.navigation.request().is_some());
    }
    #[test]
    fn stopped_growth_near_tail_keeps_up_navigation_available() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        let text = (0..100)
            .map(|i| format!("cached line {i:03}\n"))
            .collect::<String>();
        view.nodes = vec![UiNode {
            node_id: "saved".into(),
            role: "assistant".into(),
            text: text.clone(),
            total_chars: text.chars().count(),
            ..Default::default()
        }];
        view.thread_id = Some("thread".into());
        view.total_nodes = 1;
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: "cached line 000\n".len() * 80,
        };
        view.navigation.scroll.width = 80;
        view.navigation.scroll.height = 10;
        view.total_nodes = 2; // Another committed node arrives while paused.
        let mut cache = HistoryLayoutCache::default();
        let actions = view.scroll(30, "thread", &mut cache, 1024 * 1024);
        assert!(matches!(
            actions.as_slice(),
            [UiAction::LoadHistoryWindow { .. }]
        ));
        let before = view.navigation.anchor.char_offset;
        view.scroll(-1, "thread", &mut cache, 1024 * 1024);
        assert!(
            view.navigation.anchor.char_offset < before,
            "up remains usable before the read returns"
        );
        assert_eq!(
            view.navigation.pending(),
            0,
            "opposite input cancels queued forward movement"
        );
        assert_eq!(
            view.finish_navigation_reply(),
            0,
            "a late reply cannot replay the cancelled movement"
        );
    }
    #[test]
    fn rebase_reply_keeps_newer_cached_movement_on_the_same_source() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        let text = "source line\n".repeat(100);
        let node = UiNode {
            node_id: "saved".into(),
            role: "assistant".into(),
            text: text.clone(),
            total_chars: text.chars().count(),
            ..Default::default()
        };
        view.nodes = vec![node.clone()];
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 100,
        };
        view.navigation.seed_rebasing(true);
        view.navigation
            .on_event(NavEvent::Read(ReadEvent::PositionMoved));
        let old = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 200,
        };
        view.on_history_window(vec![node], 0, 0, 1, old, true);
        assert_eq!(
            view.navigation.anchor.char_offset, 100,
            "late reply must not undo cached movement"
        );
    }
    #[test]
    fn rebase_reply_uses_authoritative_offset_without_newer_movement() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        let text = "source line\n".repeat(100);
        let node = UiNode {
            node_id: "saved".into(),
            role: "assistant".into(),
            text: text.clone(),
            total_chars: text.chars().count(),
            ..Default::default()
        };
        view.nodes = vec![node.clone()];
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 100,
        };
        view.navigation.seed_rebasing(true);
        let mapped = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 200,
        };
        view.on_history_window(vec![node], 0, 0, 1, mapped, true);
        assert_eq!(view.navigation.anchor.char_offset, 200);
    }
    #[test]
    fn stream_commit_rebase_does_not_discard_queued_navigation() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.thread_id = Some("thread".into());
        view.navigation.select_reading();
        view.navigation.anchor = ViewAnchor {
            node_id: Some("saved".into()),
            char_offset: 0,
        };
        view.navigation.on_event(NavEvent::ReadRequested {
            ticket: (view.navigation.view_revision, None),
            pending: None,
            new_epoch: false,
        });
        view.navigation.seed_pending(-4);
        let action = view.rebase_committed_stream("thread".into(), view.generation);
        assert!(matches!(action, Some(UiAction::RebaseHistory { .. })));
        assert_eq!(
            view.navigation.pending(),
            -4,
            "rebase must preserve queued movement"
        );
    }
    #[test]
    fn rejected_navigation_releases_matching_read_and_discards_its_movement() {
        for node_window in [false, true] {
            let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
            ui.select_thread("thread".into());
            let generation = ui.view().generation;
            let revision = ui.view().navigation.view_revision;
            let node = node_window.then(|| "node".to_owned());
            ui.view_mut().navigation.set_loading(true);
            ui.view_mut()
                .navigation
                .set_request(Some((revision, node.clone())));
            ui.view_mut().navigation.seed_pending(-3);
            let action = if node_window {
                UiAction::LoadNodeWindow {
                    thread_id: "thread".into(),
                    generation,
                    view_revision: revision,
                    node_id: "node".into(),
                    char_offset: 0,
                }
            } else {
                UiAction::LoadHistoryWindow {
                    thread_id: "thread".into(),
                    generation,
                    view_revision: revision,
                    ordinal: 0,
                    char_offset: 0,
                    from_end: false,
                    selection_revision: 0,
                }
            };
            ui.action_rejected(&action);
            assert!(
                !ui.view().navigation.is_loading(),
                "rejected read must not leave loading set"
            );
            assert!(ui.view().navigation.request().is_none());
            assert_eq!(
                ui.view().navigation.pending(),
                0,
                "rejected displacement ends"
            );
            assert!(ui.take_deferred_actions().is_empty(), "no automatic retry");
        }
    }
    #[test]
    fn initiating_scroll_is_replayed_after_stream_rebase() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("thread".into());
        let generation = ui.view().generation;
        let text = (0..100)
            .map(|i| format!("source line {i:03}\n"))
            .collect::<String>();
        ui.apply(UiUpdate::Stream {
            thread_id: "thread".into(),
            generation,
            node_id: "attempt".into(),
            role: "assistant".into(),
            text: text.clone(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: Default::default(),
            ack: None,
        });
        let offset = "source line 000\n".len() * 50;
        ui.view_mut().navigation.select_reading();
        ui.view_mut().navigation.scroll.width = 80;
        ui.view_mut().navigation.scroll.height = 10;
        ui.view_mut().navigation.anchor = ViewAnchor {
            node_id: Some("attempt".into()),
            char_offset: offset,
        };
        ui.apply(UiUpdate::StreamCommitted {
            thread_id: "thread".into(),
            generation,
            attempt_id: "attempt".into(),
            node_id: "saved".into(),
        });
        let original = ui.take_deferred_actions().pop().unwrap();
        ui.action_rejected(&original);
        let key = ui.active.clone();
        let actions =
            ui.views
                .get_mut(&key)
                .unwrap()
                .scroll(-1, &key, &mut ui.layout_cache, 1024 * 1024);
        assert!(matches!(actions.as_slice(), [UiAction::RebaseStream(_)]));
        let revision = ui.view().navigation.view_revision;
        let saved = UiNode {
            node_id: "saved".into(),
            role: "assistant".into(),
            text: text.clone(),
            total_chars: text.chars().count(),
            ..Default::default()
        };
        ui.apply(UiUpdate::HistoryWindow {
            thread_id: "thread".into(),
            generation,
            view_revision: revision,
            nodes: vec![saved],
            pending: Default::default(),
            read_revision: 0,
            first_ordinal: 0,
            selection_revision: 0,
            total_nodes: 1,
            anchor: ViewAnchor {
                node_id: Some("saved".into()),
                char_offset: offset,
            },
        });
        assert!(!ui.view().navigation.follow());
        assert!(
            ui.view().navigation.anchor.char_offset < offset,
            "movement must survive the read"
        );
        assert_eq!(ui.view().navigation.pending(), 0);
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
        ui.view_mut().navigation.select_reading();
        ui.view_mut().navigation.anchor = ViewAnchor {
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
        assert!(!ui.view().navigation.is_loading());
        assert!(ui.view().navigation.committed_preview.is_some());
        let key = ui.active.clone();
        let retry =
            ui.views
                .get_mut(&key)
                .unwrap()
                .scroll(1, &key, &mut ui.layout_cache, 1024 * 1024);
        assert!(
            matches!(retry.as_slice(),[UiAction::RebaseStream(request)]if request.source.text=="bounded source"&&request.anchor.char_offset==7)
        );
        assert_eq!(ui.view().navigation.pending(), 1);
        let old_revision = ui.view().navigation.view_revision;
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::End,
            KeyModifiers::CONTROL,
        )));
        assert!(ui.view().navigation.follow());
        assert_ne!(ui.view().navigation.view_revision, old_revision);
        ui.apply(commit());
        assert!(!ui.view().committed_nodes.is_empty());
        assert_eq!(ui.view().nodes()[0].text, "bounded source");
        // A late accepted window from before Ctrl+End cannot pull follow back.
        ui.apply(UiUpdate::HistoryWindow {
            pending: Default::default(),
            read_revision: 0,
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
        assert!(ui.view().navigation.follow());
    }
}

impl ThreadView {
    pub(super) fn retain_incomplete_stream(&mut self, attempt: &str, source: String, kind: &str) {
        self.committed_nodes.remove(attempt);
        if self.nodes.iter().any(|n| n.node_id == source) {
            self.provisional_nodes.remove(attempt);
            self.nodes.retain(|n| n.node_id != attempt);
            if self.navigation.anchor.node_id.as_deref() == Some(attempt) {
                self.navigation.anchor.node_id = Some(source);
                self.navigation.anchor.char_offset = 0;
            }
            self.mark_source_changed();
            return;
        }
        if let Some(node) = self.nodes.iter_mut().find(|n| n.node_id == attempt) {
            self.provisional_nodes.remove(attempt);
            self.provisional_nodes.insert(source.clone());
            if self.navigation.anchor.node_id.as_deref() == Some(attempt) {
                self.navigation.anchor.node_id = Some(source.clone());
            }
            node.node_id = source;
            node.role = "received_response".into();
            // Saved Turn state is not part of StreamSettled. Keep the paid
            // preview neutral until its DB projection confirms recovery_wait.
            node.presentation.kind = crate::presentation::PresentationKind::Event;
            node.presentation.event_class = crate::presentation::EventClass::Change;
            node.presentation.title = if kind == "model_stream_observation" {
                crate::ui::text_width::glyphs::localized!(
                    self.language,
                    "ストリーム差分 · 最終応答は確定済み",
                    "ストリーム差分 | 最終応答は確定済み",
                    "Stream delta · Final response is already confirmed",
                    "Stream delta | Final response is already confirmed"
                )
            } else {
                crate::ui::text_width::glyphs::localized!(
                    self.language,
                    "途中で切れた応答（保存済み・会話には含めない） · /received 一覧",
                    "途中で切れた応答（保存済み・会話には含めない） | /received 一覧",
                    "Interrupted response (saved; not part of the conversation) · /received List",
                    "Interrupted response (saved; not part of the conversation) | /received List"
                )
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
    pub(super) fn on_incomplete_responses(&mut self, nodes: Vec<(String, UiNode)>) {
        let previous_order = self
            .nodes
            .iter()
            .map(|n| n.node_id.clone())
            .collect::<Vec<_>>();
        let ids = nodes
            .iter()
            .filter(|(after, _)| self.nodes.iter().any(|node| &node.node_id == after))
            .map(|(_, node)| node.node_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let previous = self.nodes.len();
        self.nodes
            .retain(|n| n.role != "received_response" || ids.contains(&n.node_id));
        let mut changed = previous != self.nodes.len();
        self.provisional_nodes
            .retain(|id| self.nodes.iter().any(|n| &n.node_id == id));
        // Insert backwards so previews sharing a selected predecessor retain
        // their saved chronological order. Never append to an unrelated Turn.
        for (after, mut incoming) in nodes.into_iter().rev() {
            if !ids.contains(&incoming.node_id) {
                continue;
            }
            self.provisional_nodes.insert(incoming.node_id.clone());
            let previous_index = self
                .nodes
                .iter()
                .position(|n| n.node_id == incoming.node_id);
            if let Some(index) = previous_index {
                let mut existing = self.nodes.remove(index);
                if !existing.same_projection(&incoming) {
                    changed = true;
                    if !self.navigation.follow()
                        && self.navigation.anchor.node_id.as_deref() == Some(&incoming.node_id)
                    {
                        existing.total_chars = incoming.total_chars;
                        // The saved Turn state may change without a new source
                        // window. Refresh its metadata while retaining the
                        // paused viewport's body, blocks and character offsets.
                        existing.presentation.kind = incoming.presentation.kind;
                        existing.presentation.event_class = incoming.presentation.event_class;
                        existing.presentation.title = incoming.presentation.title.clone();
                        existing.presentation.fields = incoming.presentation.fields.clone();
                        existing.presentation.projection_stamp = None;
                        incoming = existing;
                    }
                }
            }
            let index = self.nodes.iter().position(|n| n.node_id == after).unwrap() + 1;
            changed |= previous_index.is_none();
            self.nodes.insert(index, incoming);
        }
        changed |= self
            .nodes
            .iter()
            .map(|n| &n.node_id)
            .ne(previous_order.iter());
        if changed {
            self.mark_source_changed();
        }
    }
}

#[cfg(test)]
mod received_tests {
    use super::*;
    #[test]
    fn anchored_received_preview_refreshes_state_color_without_replacing_its_body_window() {
        use crate::presentation::{EventClass, PresentationField, PresentationKind};
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let generation = ui.view().generation;
        let title = "途中で切れた応答（保存済み・会話には含めない） | /received 一覧";
        let mut paid = UiNode {
            node_id: "paid".into(),
            role: "received_response".into(),
            text: "cached body window".into(),
            base_char_offset: 100,
            total_chars: 200,
            reasoning_ranges: vec![(100, 102)],
            presentation: crate::presentation::project_stream_window(
                "cached body window",
                100,
                &[(100, 102)],
                160,
            ),
        };
        paid.presentation.kind = PresentationKind::Interrupted;
        paid.presentation.event_class = EventClass::Attention;
        paid.presentation.title = title.into();
        paid.presentation.projection_stamp = Some([1; 32]);
        let body_blocks = paid.presentation.blocks.clone();
        ui.view_mut().nodes = vec![
            UiNode {
                node_id: "selected-predecessor".into(),
                role: "user".into(),
                text: "saved request".into(),
                total_chars: 13,
                ..Default::default()
            },
            paid,
        ];
        ui.view_mut().selection_revision = 7;
        ui.view_mut().received_revision = 10;
        ui.view_mut().navigation.select_reading();
        ui.view_mut().reasoning_expanded.insert("paid".into());
        ui.view_mut().navigation.anchor = ViewAnchor {
            node_id: Some("paid".into()),
            char_offset: 103,
        };
        let anchor = ui.view().navigation.anchor.clone();
        for recovery in [false, true, false] {
            // A fresh DB projection contains only the leading body window,
            // while this paused viewport is 100 characters into the same source.
            let mut incoming = UiNode {
                node_id: "paid".into(),
                role: "received_response".into(),
                text: "different leading window".into(),
                total_chars: 200,
                presentation: crate::presentation::project_stream_window(
                    "different leading window",
                    0,
                    &[],
                    160,
                ),
                ..Default::default()
            };
            incoming.presentation.kind = if recovery {
                PresentationKind::Interrupted
            } else {
                PresentationKind::Event
            };
            incoming.presentation.event_class = if recovery {
                EventClass::Attention
            } else {
                EventClass::Change
            };
            incoming.presentation.title = title.into();
            incoming.presentation.fields = vec![PresentationField {
                key: "/source_node_id".into(),
                label: "保存原文".into(),
                value: "saved-source-uuid".into(),
                ..Default::default()
            }];
            let before = ui.view().history_revision;
            ui.dirty = false;
            ui.apply(UiUpdate::IncompleteResponses {
                thread_id: "t".into(),
                generation,
                selection_revision: 7,
                view_revision: None,
                root_revision: 10,
                nodes: vec![("selected-predecessor".into(), incoming)],
            });
            let view = ui.view();
            let saved = &view.nodes[1];
            assert_eq!(saved.text, "cached body window");
            assert_eq!(saved.base_char_offset, 100);
            assert_eq!(saved.reasoning_ranges, [(100, 102)]);
            assert_eq!(saved.presentation.blocks, body_blocks);
            assert_eq!(saved.presentation.fields[0].value, "saved-source-uuid");
            assert_eq!(saved.presentation.title, title);
            assert_eq!(saved.presentation.projection_stamp, None);
            assert_eq!(view.navigation.anchor, anchor);
            assert_eq!(view.received_revision, 10);
            assert!(view.history_revision > before);
            assert!(ui.dirty);
            let lines = presentation::history(view, 160).0;
            assert!(
                presentation::plain(&lines).contains("body window"),
                "a neutral audit preview must retain its body"
            );
            let heading = lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.contains("途中で切れた応答"))
                .unwrap();
            assert_eq!(
                heading.style.fg,
                Some(if recovery {
                    theme::ATTENTION
                } else {
                    theme::MUTED
                })
            );
        }
    }

    #[test]
    fn saved_previews_stay_in_their_turn_and_do_not_leak_into_other_pages() {
        fn node(id: &str, role: &str) -> UiNode {
            UiNode {
                node_id: id.into(),
                role: role.into(),
                text: id.into(),
                total_chars: id.len(),
                ..Default::default()
            }
        }
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.nodes = vec![
            node("old-user", "user"),
            node("old-end", "event"),
            node("new-user", "user"),
            node("new-answer", "assistant"),
        ];
        let previews = || {
            vec![
                ("old-end".into(), node("paid-a", "received_response")),
                ("old-end".into(), node("paid-b", "received_response")),
            ]
        };
        view.on_incomplete_responses(previews());
        let ids = |view: &ThreadView| {
            view.nodes
                .iter()
                .map(|node| node.node_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&view),
            [
                "old-user",
                "old-end",
                "paid-a",
                "paid-b",
                "new-user",
                "new-answer"
            ]
        );
        view.on_incomplete_responses(previews());
        assert_eq!(
            ids(&view),
            [
                "old-user",
                "old-end",
                "paid-a",
                "paid-b",
                "new-user",
                "new-answer"
            ]
        );
        view.nodes.retain(|node| {
            node.node_id == "new-user"
                || node.node_id == "new-answer"
                || node.role == "received_response"
        });
        view.on_incomplete_responses(previews());
        assert_eq!(ids(&view), ["new-user", "new-answer"]);
        assert!(!view.provisional_nodes.contains("paid-a"));
        // Returning to the old page restores the real source after its own
        // selected predecessor, without changing canonical history ordinals.
        view.nodes.insert(0, node("old-end", "event"));
        view.on_incomplete_responses(previews());
        assert_eq!(
            ids(&view),
            ["old-end", "paid-a", "paid-b", "new-user", "new-answer"]
        );
    }
    #[test]
    fn received_previews_do_not_advance_selected_ordinals_or_break_page_merges() {
        let node = |id: &str| UiNode {
            node_id: id.into(),
            role: "assistant".into(),
            text: id.into(),
            total_chars: id.len(),
            ..Default::default()
        };
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.nodes = vec![node("n0"), node("n1"), node("n2")];
        view.first_ordinal = 10;
        let mut paid = node("paid");
        paid.role = "received_response".into();
        view.on_incomplete_responses(vec![("n0".into(), paid.clone())]);
        assert_eq!(view.selected_node_count(), 3);
        let mut incoming = vec![node("n1"), node("n2"), node("n3")];
        let mut first = 11;
        view.merge_navigation_window(&mut incoming, &mut first);
        assert_eq!(first, 10);
        assert_eq!(
            incoming
                .iter()
                .map(|node| node.node_id.as_str())
                .collect::<Vec<_>>(),
            ["n0", "n1", "n2", "n3"]
        );
        view.nodes = vec![paid, node("n1")];
        view.provisional_nodes.insert("paid".into());
        let mut retained = usize::MAX;
        view.trim_page_edges(&["n1".into()], &mut retained, 0);
        assert_eq!(view.first_ordinal, 10, "a preview has no canonical ordinal");
        assert_eq!(view.selected_node_count(), 1);
        assert!(!view.provisional_nodes.contains("paid"));
    }
    #[test]
    fn delayed_received_refresh_cannot_replace_a_newer_snapshot_or_navigation() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let generation = ui.view().generation;
        let node = |id: &str, role: &str| UiNode {
            node_id: id.into(),
            role: role.into(),
            text: id.into(),
            total_chars: id.len(),
            ..Default::default()
        };
        ui.view_mut().nodes = vec![node("anchor", "assistant")];
        ui.view_mut().selection_revision = 7;
        ui.view_mut().navigation.view_revision = 5;
        ui.view_mut().received_window_revision = Some(5);
        let refresh = |selection_revision, view_revision, root_revision, nodes| {
            UiUpdate::IncompleteResponses {
                thread_id: "t".into(),
                generation,
                selection_revision,
                view_revision,
                root_revision,
                nodes,
            }
        };
        ui.apply(refresh(
            7,
            Some(5),
            10,
            vec![("anchor".into(), node("paid", "received_response"))],
        ));
        assert!(ui.view().nodes.iter().any(|n| n.node_id == "paid"));
        for stale in [
            refresh(7, Some(5), 9, vec![]),
            refresh(7, Some(4), 11, vec![]),
            refresh(6, None, 12, vec![]),
        ] {
            ui.apply(stale);
            assert!(ui.view().nodes.iter().any(|n| n.node_id == "paid"));
            assert_eq!(ui.view().received_revision, 10);
        }
        ui.apply(refresh(7, Some(5), 11, vec![]));
        assert!(!ui.view().nodes.iter().any(|n| n.node_id == "paid"));
        assert_eq!(ui.view().received_revision, 11);
    }
    #[test]
    fn received_window_scope_rejects_stale_replies_but_retains_paid_body_until_fresh_snapshot() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let generation = ui.view().generation;
        let node = |id: &str, role: &str| UiNode {
            node_id: id.into(),
            role: role.into(),
            text: id.into(),
            total_chars: id.len(),
            ..Default::default()
        };
        let window = |revision, selection_revision| UiUpdate::HistoryWindow {
            pending: Default::default(),
            read_revision: 0,
            thread_id: "t".into(),
            generation,
            view_revision: revision,
            nodes: vec![node("anchor", "assistant"), node("next", "assistant")],
            first_ordinal: 0,
            selection_revision,
            total_nodes: 2,
            anchor: ViewAnchor {
                node_id: Some("anchor".into()),
                char_offset: 0,
            },
        };
        let received = |revision, root_revision| UiUpdate::IncompleteResponses {
            thread_id: "t".into(),
            generation,
            selection_revision: 7,
            view_revision: Some(revision),
            root_revision,
            nodes: vec![("anchor".into(), node("paid", "received_response"))],
        };
        ui.view_mut().navigation.select_reading();
        ui.view_mut().selection_revision = 7;
        ui.view_mut().navigation.view_revision = 5;
        ui.view_mut().navigation.set_request(Some((5, None)));
        ui.view_mut().navigation.seed_pending(1);
        // Before its saved-history window is applied, the corresponding audit
        // must not install a source into an unrelated frame.
        ui.apply(received(5, 10));
        assert_eq!(ui.view().received_revision, 0);
        ui.apply(window(5, 7));
        assert_eq!(
            ui.view().navigation.view_revision,
            6,
            "pending scroll resumes immediately"
        );
        assert_eq!(ui.view().received_window_revision, Some(5));
        ui.apply(received(5, 10));
        assert!(ui.view().nodes.iter().any(|node| node.node_id == "paid"));
        // An in-flight newer request still shows the old applied window. Its
        // received source remains valid until the replacement window arrives.
        ui.view_mut().navigation.set_request(Some((6, None)));
        ui.apply(received(5, 11));
        assert_eq!(ui.view().received_revision, 11);
        ui.apply(window(6, 7));
        assert_eq!(ui.view().received_window_revision, Some(6));
        ui.apply(received(5, 12));
        assert!(ui.view().nodes.iter().any(|node| node.node_id == "paid"));
        assert_eq!(ui.view().received_revision, 11);
        ui.apply(UiUpdate::IncompleteResponses {
            thread_id: "t".into(),
            generation,
            selection_revision: 7,
            view_revision: Some(6),
            root_revision: 12,
            nodes: vec![],
        });
        assert!(!ui.view().nodes.iter().any(|node| node.node_id == "paid"));
        assert_eq!(ui.view().received_revision, 12);
        ui.apply(received(6, 13));
        assert!(ui.view().nodes.iter().any(|node| node.node_id == "paid"));
        // A delayed window cannot move the accepted selection backwards.
        let revision = ui.view().navigation.view_revision;
        ui.apply(window(revision, 6));
        assert_eq!(ui.view().selection_revision, 7);
        assert_eq!(ui.view().received_window_revision, Some(6));
    }
    #[test]
    fn unsaved_conversation_title_follows_language_without_creating_a_thread() {
        for (language, title) in [
            (crate::config::UiLanguage::Ja, "新しい会話"),
            (crate::config::UiLanguage::En, "New conversation"),
        ] {
            let mut config = Config::default();
            config.ui.language = language;
            let ui = UiState::new(&config, std::env::temp_dir());
            assert_eq!(ui.view().title, title);
            assert!(ui.view().thread_id.is_none());
        }
    }

    #[test]
    fn settled_stream_titles_are_localized_without_changing_raw_text_or_identity() {
        use crate::config::UiLanguage::{En, Ja};
        use crate::ui::text_width;
        for language in [Ja, En] {
            for policy in [Some(1), Some(2), None] {
                text_width::set_terminal_width(policy);
                for (kind, ja, en) in [
                    ("model_stream_observation", "ストリーム差分", "Stream delta"),
                    (
                        "incomplete_response",
                        "途中で切れた応答",
                        "Interrupted response",
                    ),
                ] {
                    let mut config = Config::default();
                    config.ui.language = language;
                    let mut ui = UiState::new(&config, std::env::temp_dir());
                    ui.view_mut().nodes.push(UiNode {
                        node_id: "attempt-raw".into(),
                        role: "assistant".into(),
                        text: "raw 日本語".into(),
                        total_chars: 7,
                        ..Default::default()
                    });
                    ui.view_mut().provisional_nodes.insert("attempt-raw".into());
                    ui.view_mut().retain_incomplete_stream(
                        "attempt-raw",
                        "source-raw".into(),
                        kind,
                    );
                    let node = &ui.view().nodes()[0];
                    assert_eq!(node.node_id, "source-raw");
                    assert_eq!(node.role, "received_response");
                    assert_eq!(node.text, "raw 日本語");
                    assert!(
                        node.presentation
                            .title
                            .contains(if language == Ja { ja } else { en })
                    );
                    assert_eq!(node.presentation.title.contains('·'), policy == Some(1));
                    if kind == "incomplete_response" {
                        assert!(node.presentation.title.contains("/received"));
                    }
                }
            }
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn terminal_failure_keeps_visible_paid_text_and_fresh_generations_own_saved_previews() {
        let mut config = Config::default();
        config.ui.language = crate::config::UiLanguage::Ja;
        let mut ui = UiState::new(&config, std::env::temp_dir());
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
        assert!(
            ui.view().nodes()[0]
                .presentation
                .title
                .contains("途中で切れた応答")
        );
        assert_eq!(
            ui.view().nodes()[0].presentation.kind,
            crate::presentation::PresentationKind::Event
        );
        assert_eq!(
            ui.view().nodes()[0].presentation.event_class,
            crate::presentation::EventClass::Change
        );
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
            selection_revision: 0,
            view_revision: None,
            root_revision: 0,
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

        let revision = view.navigation.view_revision;
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
        let pending_revision = view.navigation.view_revision;
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

fn node_storage_bytes(node: &UiNode) -> usize {
    node.metadata_bytes() - std::mem::size_of::<UiNode>() + node.cached_bytes()
}

#[cfg(test)]
mod body_accounting_tests {
    use super::*;
    #[test]
    fn streaming_replacement_eviction_and_capacity_compaction_keep_exact_body_totals() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        for n in 0..24 {
            let incoming = UiNode {
                node_id: "live".into(),
                role: "assistant".into(),
                text: "日本語".repeat(n + 1),
                total_chars: (n + 1) * 3,
                ..Default::default()
            };
            view.on_stream(incoming);
            let expected = view.nodes.capacity() * std::mem::size_of::<UiNode>()
                + view.nodes.iter().map(node_storage_bytes).sum::<usize>();
            assert_eq!(view.retained_body_bytes(&[]), expected);
            let exempt = vec!["live".into()];
            assert_eq!(
                view.retained_body_bytes(&exempt),
                view.nodes.capacity().saturating_sub(1) * std::mem::size_of::<UiNode>()
            );
        }
        view.evict_node_body(0);
        view.compact_node_slots();
        assert_eq!(
            view.retained_body_bytes(&[]),
            view.nodes.capacity() * std::mem::size_of::<UiNode>()
                + view.nodes.iter().map(node_storage_bytes).sum::<usize>()
        );
        view.discard_cached_history();
        assert_eq!(view.retained_body_bytes(&[]), 0);
    }
    #[test]
    fn local_tool_observation_failure_invalidates_projection_stamp() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        let mut node = UiNode {
            node_id: "tool".into(),
            ..Default::default()
        };
        node.presentation.projection_stamp = Some([7; 32]);
        node.presentation
            .blocks
            .push(crate::presentation::PresentationBlock {
                tool: Some(crate::presentation::ToolPresentation::default()),
                ..Default::default()
            });
        view.nodes.push(node);
        view.on_tool_facts(Err("not currently observable".into()));
        assert!(view.nodes[0].presentation.projection_stamp.is_none());
    }
    #[test]
    fn paused_stream_window_extent_changes_render_unit_identity_without_losing_old_text() {
        let mut view = ThreadView::new(&Config::default(), std::env::temp_dir());
        view.navigation.select_reading();
        view.navigation.anchor.node_id = Some("live".into());
        view.navigation.anchor.char_offset = 0;
        let mut original = UiNode {
            node_id: "live".into(),
            role: "assistant".into(),
            text: "old text".into(),
            total_chars: 8,
            ..Default::default()
        };
        original.presentation.projection_stamp = Some([7; 32]);
        view.nodes.push(original);
        let old = RenderUnitKey::new(&view, 0, 1, 80, &[]).unwrap();
        let mut incoming = UiNode {
            node_id: "live".into(),
            role: "assistant".into(),
            text: "new tail".into(),
            base_char_offset: 100,
            total_chars: 108,
            ..Default::default()
        };
        incoming.presentation.projection_stamp = Some([8; 32]);
        view.on_stream(incoming);
        assert_eq!(view.nodes[0].text, "old text");
        assert_eq!(view.nodes[0].presentation.projection_stamp, Some([7; 32]));
        assert_ne!(old, RenderUnitKey::new(&view, 0, 1, 80, &[]).unwrap());
    }
}

#[cfg(test)]
mod retention_regressions {
    use super::*;

    fn node(id: &str, text: &str, read: u64) -> UiNode {
        UiNode {
            node_id: id.into(),
            text: text.into(),
            total_chars: text.chars().count(),
            presentation: NodePresentation {
                read_revision: read,
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn view() -> ThreadView {
        let mut view = ThreadView::new(&Config::default(), PathBuf::new());
        view.thread_id = Some("thread".into());
        view
    }
    #[test]
    fn backward_window_intent_is_independent_of_initial_tail_hydration_order() {
        let window = |range: std::ops::Range<u64>| {
            range
                .map(|ordinal| node(&format!("n{ordinal}"), "saved body", 1))
                .collect::<Vec<_>>()
        };
        for hydrated in [false, true] {
            let mut following = view();
            following.on_history(
                if hydrated { window(11..61) } else { vec![] },
                None,
                false,
                11,
                1,
                61,
            );
            let fallback = following.on_history_window(
                window(0..50),
                0,
                1,
                61,
                ViewAnchor {
                    node_id: Some("n0".into()),
                    char_offset: 0,
                },
                true,
            );
            assert_eq!(
                fallback, !hydrated,
                "following may only accept a window that still covers its tail"
            );
            assert_eq!(
                following.nodes().iter().any(|n| n.node_id == "n0"),
                hydrated
            );

            let mut paused = view();
            paused.on_history(
                if hydrated { window(11..61) } else { vec![] },
                None,
                false,
                11,
                1,
                61,
            );
            paused.navigation.select_reading();
            paused.navigation.anchor = ViewAnchor {
                node_id: Some("n0".into()),
                char_offset: 0,
            };
            assert!(!paused.on_history_window(
                window(0..50),
                0,
                1,
                61,
                paused.navigation.anchor.clone(),
                true
            ));
            assert!(paused.nodes().iter().any(|n| n.node_id == "n0"));
        }
    }

    #[test]
    fn follow_window_keeps_paid_stream_until_canonical_body_is_installed() {
        let mut view = view();
        view.on_history(vec![node("old", "previous Turn", 1)], None, false, 0, 1, 1);
        view.on_stream(node("attempt", "current paid response", 0));
        view.commit_stream_node("attempt".into(), "saved".into(), "thread".into(), 0);
        let mut unloaded = node("saved", "", 2);
        unloaded.total_chars = 21;
        assert!(!view.on_history_window(
            vec![node("old", "previous Turn", 2), unloaded],
            0,
            2,
            2,
            ViewAnchor::default(),
            false
        ));
        assert!(view.navigation.follow());
        assert!(
            view.nodes
                .iter()
                .any(|n| n.node_id == "attempt" && n.text == "current paid response")
        );
        assert!(view.committed_nodes.contains_key("attempt"));
        view.on_history_window(
            vec![
                node("old", "previous Turn", 3),
                node("saved", "current paid response", 3),
            ],
            0,
            2,
            2,
            ViewAnchor::default(),
            false,
        );
        assert!(view.navigation.follow());
        assert!(!view.nodes.iter().any(|n| n.node_id == "attempt"));
        assert!(
            view.nodes
                .iter()
                .any(|n| n.node_id == "saved" && n.text == "current paid response")
        );
    }
    #[test]
    fn disjoint_visible_and_current_working_regions_keep_both_page_edges() {
        let mut view = view();
        view.on_history(
            (0..8)
                .map(|i| node(&format!("n{i}"), "cached body", 1))
                .collect(),
            None,
            false,
            0,
            1,
            8,
        );
        let mut retained = usize::MAX / 2;
        view.trim_page_edges(&["n0".into(), "n7".into()], &mut retained, 0);
        assert_eq!(view.nodes.first().unwrap().node_id, "n0");
        assert_eq!(view.nodes.last().unwrap().node_id, "n7");
    }
    #[test]
    fn only_offscreen_committed_previews_are_discarded_as_complete_units() {
        let mut view = view();
        view.on_history(vec![node("base", "saved", 1)], None, false, 0, 1, 1);
        for (attempt, saved) in [("shown", "saved-shown"), ("offscreen", "saved-offscreen")] {
            view.on_stream(node(attempt, "paid output", 0));
            view.commit_stream_node(attempt.into(), saved.into(), "thread".into(), 0);
        }
        view.on_stream(node("inflight", "uncommitted output", 0));
        view.discard_committed_previews_except(&["shown".into(), "inflight".into()]);
        assert!(
            view.nodes
                .iter()
                .any(|n| n.node_id == "shown" && !n.text.is_empty())
        );
        assert!(
            view.nodes
                .iter()
                .any(|n| n.node_id == "inflight" && !n.text.is_empty())
        );
        assert!(!view.nodes.iter().any(|n| n.node_id == "offscreen"));
        assert!(!view.committed_nodes.contains_key("offscreen"));
    }
    #[test]
    fn overlapping_follow_windows_preserve_uncommitted_streams() {
        for total in [2, 3] {
            let mut view = view();
            view.on_history(
                vec![node("a", "first", 1), node("b", "second", 1)],
                None,
                false,
                0,
                1,
                total,
            );
            view.on_stream(node("live", "paid uncommitted stream", 0));
            view.on_history_window(
                vec![node("a", "first", 2), node("b", "second", 2)],
                0,
                1,
                total,
                ViewAnchor::default(),
                false,
            );
            assert!(view.navigation.follow());
            assert!(
                view.nodes
                    .iter()
                    .any(|n| n.node_id == "live" && n.text == "paid uncommitted stream")
            );
            assert!(view.nodes.iter().any(|n| n.node_id == "b"));
        }
    }
    #[test]
    fn following_non_tail_anchor_reply_retains_current_frame_and_requests_tail() {
        let mut view = view();
        view.on_history(
            vec![node("latest", "current Turn tool", 10)],
            None,
            false,
            9,
            1,
            10,
        );
        view.on_stream(node("stream", "visible stream", 0));
        let refresh = view.on_history_window(
            vec![node("old", "old Turn", 11)],
            0,
            1,
            10,
            ViewAnchor::default(),
            false,
        );
        assert!(refresh);
        assert!(view.navigation.follow());
        assert!(view.nodes.iter().any(|n| n.node_id == "latest"));
        assert!(view.nodes.iter().any(|n| n.node_id == "stream"));
    }
    #[test]
    fn node_hydration_order_is_separate_from_queue_age_and_navigation_intent() {
        let mut view = view();
        view.on_history(vec![node("n", "new window", 10)], None, false, 0, 1, 1);
        view.delivery_read_revision = 100;
        view.on_node_window(0, node("n", "stale window", 5));
        assert_eq!(view.nodes[0].text, "new window");
        view.on_node_window(0, node("n", "needed hydration", 11));
        assert_eq!(view.nodes[0].text, "needed hydration");
        assert_eq!(view.delivery_read_revision, 100);
        view.navigation.select_reading();
        view.navigation.set_request(Some((0, Some("n".into()))));
        let mut navigation = node("n", "earlier source offset", 4);
        navigation.base_char_offset = 42;
        view.on_node_window(0, navigation);
        assert_eq!(view.nodes[0].base_char_offset, 0);
        assert_eq!(view.nodes[0].text, "needed hydration");
        let mut retry = node("n", "earlier source offset", 12);
        retry.base_char_offset = 42;
        view.on_node_window(0, retry);
        assert_eq!(view.nodes[0].base_char_offset, 42);
        assert_eq!(view.nodes[0].presentation.read_revision, 12);
    }
    #[test]
    fn discarded_failed_raw_previews_cannot_move_the_retained_tool_body() {
        use crate::presentation::{
            BlockKind, OutputStream, PresentationBlock, PresentationField, PresentationKind,
        };
        for (read, rebase) in [(9, false), (11, true)] {
            let mut view = view();
            let mut saved = node("tool", "same canonical source", 10);
            saved.presentation.kind = PresentationKind::ToolResult;
            saved.presentation.blocks = vec![PresentationBlock {
                kind: BlockKind::ToolResult,
                stream: Some(OutputStream::Stdout),
                text: "visible paid tail".into(),
                ..Default::default()
            }];
            view.on_history(vec![saved], None, false, 0, 1, 1);
            view.navigation.seed_rebasing(rebase);
            let mut failed = node("tool", "same canonical source", read);
            failed.presentation.kind = PresentationKind::ToolResult;
            failed.presentation.fields = vec![PresentationField {
                key: "/ui_raw_stream_preview_error".into(),
                value: "IoFailed".into(),
                ..Default::default()
            }];
            view.on_history(vec![failed], None, false, 0, 1, 1);
            assert!(
                view.nodes[0]
                    .presentation
                    .blocks
                    .iter()
                    .any(|b| b.text == "visible paid tail")
            );
        }
    }
    #[test]
    fn navigation_keeps_newer_queue_snapshot_and_releases_its_slot() {
        let mut ui = UiState::new(&Config::default(), PathBuf::new());
        ui.view_mut().thread_id = Some("thread".into());
        ui.view_mut().navigation.select_reading();
        ui.view_mut()
            .on_history(vec![node("n", "body", 20)], None, false, 0, 1, 1);
        ui.view_mut().delivery_read_revision = 20;
        ui.view_mut().pending_messages.total = 7;
        ui.view_mut().navigation.set_request(Some((0, None)));
        ui.view_mut().navigation.set_loading(true);
        ui.apply(UiUpdate::HistoryWindow {
            thread_id: "thread".into(),
            generation: 0,
            view_revision: 0,
            nodes: vec![node("n", "old body", 10)],
            pending: Default::default(),
            read_revision: 10,
            first_ordinal: 0,
            selection_revision: 1,
            total_nodes: 1,
            anchor: ViewAnchor {
                node_id: Some("n".into()),
                char_offset: 0,
            },
        });
        assert_eq!(ui.view().pending_messages.total, 7);
        assert_eq!(ui.view().delivery_read_revision, 20);
        assert!(ui.view().navigation.request().is_none());
        assert!(!ui.view().navigation.is_loading());
        assert!(!ui.view().navigation.follow());
        assert_eq!(ui.view().nodes[0].text, "body");
    }
}

#[cfg(test)]
mod received_window_retention_tests {
    use super::*;

    fn node(id: &str, role: &str) -> UiNode {
        let text = format!("paid body {id}");
        UiNode {
            node_id: id.into(),
            role: role.into(),
            total_chars: text.chars().count(),
            text,
            ..Default::default()
        }
    }
    fn view(follow: bool) -> ThreadView {
        let mut view = ThreadView::new(&Config::default(), PathBuf::new());
        view.thread_id = Some("thread".into());
        view.on_history(
            vec![
                node("a", "assistant"),
                node("b", "assistant"),
                node("tail", "assistant"),
            ],
            None,
            false,
            0,
            1,
            3,
        );
        // Preserve every currently retained audit until its authoritative
        // snapshot arrives, even if prior projections temporarily retained >2.
        view.on_incomplete_responses(vec![
            ("a".into(), node("paid-a1", "received_response")),
            ("a".into(), node("paid-a2", "received_response")),
            ("b".into(), node("paid-b", "received_response")),
        ]);
        if follow {
            view.navigation.request_tail();
        } else {
            view.navigation.select_reading();
        }
        view.navigation.anchor = ViewAnchor {
            node_id: Some("b".into()),
            char_offset: 2,
        };
        view.navigation.seed_rebasing(!follow);
        view
    }
    fn ids(view: &ThreadView) -> Vec<&str> {
        view.nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect()
    }
    fn same_window(follow: bool) {
        let mut view = view(follow);
        let anchor = view.navigation.anchor.clone();
        assert!(!view.on_history_window(
            vec![
                node("a", "assistant"),
                node("b", "assistant"),
                node("tail", "assistant")
            ],
            0,
            1,
            3,
            anchor.clone(),
            true
        ));
        assert_eq!(
            ids(&view),
            ["a", "paid-a1", "paid-a2", "b", "paid-b", "tail"],
            "known audits stay after their real predecessor, not the new tail"
        );
        assert_eq!(view.navigation.follow(), follow);
        if !follow {
            assert_eq!(view.navigation.anchor, anchor);
        }
        for id in ["paid-a1", "paid-a2", "paid-b"] {
            let retained = view.nodes.iter().find(|node| node.node_id == id).unwrap();
            assert_eq!(retained.text, format!("paid body {id}"));
            assert!(view.provisional_nodes.contains(id));
        }
        // Only the subsequent accepted audit projection can declare that no
        // audits remain; history-window delivery alone never made that claim.
        view.on_incomplete_responses(vec![]);
        assert_eq!(ids(&view), ["a", "b", "tail"]);
    }
    #[test]
    fn following_same_window_retains_all_anchored_audits_until_audit_snapshot() {
        same_window(true);
    }
    #[test]
    fn paused_same_window_retains_all_anchored_audits_at_the_original_predecessors() {
        same_window(false);
    }
    #[test]
    fn ordinary_tail_refresh_keeps_audits_at_their_anchors_and_drops_missing_anchors() {
        let mut view = view(true);
        assert!(view.on_history(
            vec![
                node("a", "assistant"),
                node("b", "assistant"),
                node("tail", "assistant"),
                node("new-tail", "assistant")
            ],
            None,
            false,
            0,
            2,
            4
        ));
        assert_eq!(
            ids(&view),
            ["a", "paid-a1", "paid-a2", "b", "paid-b", "tail", "new-tail"]
        );
        assert!(view.navigation.follow());
        assert!(view.on_history(
            vec![node("tail", "assistant"), node("new-tail", "assistant")],
            None,
            false,
            2,
            2,
            4
        ));
        assert_eq!(ids(&view), ["tail", "new-tail"]);
        assert!(view.provisional_nodes.is_empty());
        assert!(view.navigation.follow());
    }
    #[test]
    fn non_tail_fallback_does_not_extract_audits_and_new_window_drops_only_missing_anchors() {
        let mut following = view(true);
        let before = ids(&following)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(following.on_history_window(
            vec![node("a", "assistant"), node("b", "assistant")],
            0,
            1,
            4,
            ViewAnchor::default(),
            true
        ));
        assert_eq!(
            ids(&following),
            before.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(
            following
                .nodes
                .iter()
                .filter(|node| node.role == "received_response")
                .all(|node| !node.text.is_empty())
        );
        let mut paused = view(false);
        let anchor = paused.navigation.anchor.clone();
        assert!(!paused.on_history_window(
            vec![node("b", "assistant"), node("tail", "assistant")],
            1,
            1,
            3,
            anchor,
            true
        ));
        assert_eq!(ids(&paused), ["b", "paid-b", "tail"]);
        assert!(paused.provisional_nodes.contains("paid-b"));
        assert!(!paused.provisional_nodes.contains("paid-a1"));
        assert!(!paused.provisional_nodes.contains("paid-a2"));
    }
}

#[cfg(test)]
mod footer85_tests {
    use super::*;
    #[test]
    fn send_stage_age_does_not_use_older_reflection_and_duplicate_ack_does_not_reset() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        let draft = ui.view().draft.clone();
        let now = Instant::now();
        let old = now - Duration::from_secs(120);
        let recent = now - Duration::from_secs(5);
        for (revision, status, at) in [
            (1, ReceiptStatus::Accepted, old),
            (2, ReceiptStatus::Accepting, recent),
        ] {
            ui.active_view_mut().pending.insert(
                revision,
                PendingSend {
                    cleared_draft: None,
                    restored_revision: None,
                    snapshot: draft.clone(),
                    status,
                    message: None,
                    source_node_id: None,
                    operation_id: None,
                    send: Some(SendProgress {
                        started_at: at,
                        reflected: false,
                    }),
                },
            );
        }
        assert_eq!(
            ui.view().sending(),
            Some((activity::ActivityPhase::Sending, 2, recent))
        );
        let view = ui.active_view_mut();
        view.on_receipt(
            draft.draft_id.clone(),
            2,
            None,
            ReceiptStatus::Accepted,
            None,
        );
        let observed = view.pending[&2].send.as_ref().unwrap().started_at;
        assert_ne!(observed, recent);
        view.on_receipt(
            draft.draft_id.clone(),
            2,
            None,
            ReceiptStatus::Accepted,
            None,
        );
        assert_eq!(view.pending[&2].send.as_ref().unwrap().started_at, observed);
        assert_eq!(
            view.sending(),
            Some((activity::ActivityPhase::Reflecting, 2, old))
        );
    }
}
