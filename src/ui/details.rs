//! Transient selected-node detail navigation; canonical viewport/draft stay unchanged.
use super::popup::Item as ListItem;
use super::theme;
use super::{ThreadView, UiAction, UiNode, popup};
use crate::{
    presentation::{PresentationField, PresentationKind},
    protocol::HistoryBody,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailSourceFacts {
    pub node_id: String,
    pub kind: PresentationKind,
    pub fields: Vec<PresentationField>,
    pub links: Vec<crate::presentation::PresentationLink>,
    pub unavailable_reason: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailSourceSnapshot {
    pub read_revision: u64,
    pub observed_at: String,
    pub sources: Vec<DetailSourceFacts>,
}
#[derive(Clone, Copy)]
enum Mode {
    Nodes,
    Fields,
    Links,
    Original,
}
pub(super) struct Details {
    pub node_id: String,
    source_node_ids: Vec<String>,
    item_key: Option<super::ItemKey>,
    item_history_revision: Option<u64>,
    item_scoped: bool,
    source_kind: PresentationKind,
    source_snapshot: Option<DetailSourceSnapshot>,
    sources_dirty: bool,
    sources_stale: Option<String>,
    last_sources_request: Option<Instant>,
    selected_field_key: Option<String>,
    selected_link_url: Option<String>,
    links_pending: bool,
    links_request_id: u64,
    links_error: Option<String>,
    mode: Mode,
    selected: usize,
    internal_fields: bool,
    scroll: Cell<u16>,
    page: Cell<usize>,
    pub request_id: u64,
    original: Option<HistoryBody>,
    original_observed_at: Option<String>,
    original_lines: Vec<Line<'static>>,
    revision: u64,
}
pub(super) enum Outcome {
    None,
    Close,
    Action(Box<UiAction>),
    Operation(super::operations::OpKind),
}
fn safe(text: &str) -> String {
    crate::presentation::sanitize_plain(text, 0).0
}
#[cfg(test)]
thread_local! { static ORIGINAL_PREPARATIONS: Cell<usize> = const { Cell::new(0) }; }
pub(super) fn original_page_lines(body: &HistoryBody) -> Vec<Line<'static>> {
    #[cfg(test)]
    ORIGINAL_PREPARATIONS.with(|count| count.set(count.get() + 1));
    let mut lines = Vec::new();
    lines.push(Line::from(format!(
        "{} · {}",
        safe(&body.kind),
        safe(&body.node_id)
    )));
    if let Some(reason) = &body.tombstone_reason {
        lines.push(Line::from(format!("原文回収済み: {}", safe(reason))));
    }
    lines.extend(
        safe(&body.text)
            .lines()
            .map(|line| Line::from(line.to_owned())),
    );
    lines.push(Line::from(safe(&format!(
        "前 {} / 後 {} / 出典 {}",
        body.previous_node_ids.join(", "),
        body.next_node_ids.join(", "),
        body.source_node_ids.join(", ")
    ))));
    for line in &mut lines {
        crate::presentation::expand_tabs(line);
    }
    lines
}
fn entry(node: &super::UiNode, width: usize) -> (String, String) {
    use crate::presentation::{BlockKind, PresentationKind};
    let has_call = node
        .presentation
        .blocks
        .iter()
        .any(|b| b.kind == BlockKind::ToolCall);
    let label = match node.presentation.kind {
        PresentationKind::User => "あなた".to_owned(),
        PresentationKind::Assistant if has_call => "モデル · shell呼出し".into(),
        PresentationKind::Assistant
            if node
                .presentation
                .blocks
                .iter()
                .any(|b| b.kind == BlockKind::Reasoning)
                && !node
                    .presentation
                    .blocks
                    .iter()
                    .any(|b| b.kind == BlockKind::Prose) =>
        {
            "モデル · 推論".into()
        }
        PresentationKind::Assistant => "モデルの回答".into(),
        PresentationKind::ToolCall => "shell呼出し".into(),
        PresentationKind::ToolResult => format!("shell結果 · {}", safe(&node.presentation.title)),
        PresentationKind::Event => format!("状態変更 · {}", safe(&node.presentation.title)),
        PresentationKind::Summary => "会話の要約".into(),
        PresentationKind::Spill
            if node.presentation.title == crate::presentation::CONTEXT_TRIM_TITLE =>
        {
            crate::presentation::CONTEXT_TRIM_TITLE.into()
        }
        PresentationKind::Spill => "原文への参照".into(),
        PresentationKind::Interrupted if !node.presentation.title.is_empty() => {
            safe(&node.presentation.title)
        }
        PresentationKind::Interrupted => "中断情報".into(),
        PresentationKind::Unknown => match node.role.as_str() {
            "user" => "あなた".into(),
            "assistant" => "モデルの回答".into(),
            "tool" => "shell結果".into(),
            _ => "状態・その他".into(),
        },
    };
    // Projection blocks contain readable commands/output, unlike canonical JSON envelopes.
    let source = node
        .presentation
        .blocks
        .iter()
        .find(|b| b.kind == BlockKind::ToolCall && !b.text.trim().is_empty())
        .or_else(|| {
            node.presentation
                .blocks
                .iter()
                .find(|b| b.kind == BlockKind::Prose && !b.text.trim().is_empty())
        })
        .or_else(|| {
            node.presentation
                .blocks
                .iter()
                .find(|b| !b.text.trim().is_empty())
        });
    let raw = source.map_or(node.text.as_str(), |block| block.text.as_str());
    let context = if matches!(node.presentation.kind, PresentationKind::Event) {
        node.presentation
            .fields
            .iter()
            .filter(|field| {
                matches!(
                    field.key.rsplit(['/', '.']).next().unwrap_or(""),
                    "model" | "model_id" | "cwd" | "state" | "status" | "reason"
                )
            })
            .take(3)
            .map(|field| {
                format!(
                    "{}: {}",
                    field.label,
                    field.value.chars().take(120).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        String::new()
    };
    let bounded: String = if context.is_empty() {
        raw.chars().take(512).collect()
    } else {
        context
    };
    let mut preview = super::threads::plain_preview(&bounded, width.min(80));
    if source.is_none()
        && matches!(node.role.as_str(), "tool" | "developer" | "system")
        && bounded.trim_start().starts_with('{')
    {
        preview = "構造化データ · Enterで詳細を確認".into();
    }
    if preview.is_empty() {
        preview = if node.total_chars > 0 {
            "本文未読込 · Oで原文を確認"
        } else {
            "本文なし · Enterで詳細を確認"
        }
        .into();
    }
    (label, preview)
}
fn source_revision(view: &ThreadView, node_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    if let Some(node) = view.nodes().iter().find(|node| node.node_id == node_id) {
        node.text.hash(&mut hash);
        node.base_char_offset.hash(&mut hash);
        node.total_chars.hash(&mut hash);
    }
    hash.finish()
}
// Human field selection is a UI projection; raw/source metadata stays on the node.
fn conversation_fields(node: &UiNode) -> Vec<PresentationField> {
    let fields = &node.presentation.fields;
    if node.presentation.kind != PresentationKind::Assistant {
        return fields.clone();
    }
    let values = fields
        .iter()
        .map(|field| (field.key.as_str(), field.value.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let value = |key: &str| values.get(key).copied();
    let messages = fields
        .iter()
        .filter_map(|field| {
            let prefix = field.key.strip_suffix("/type")?;
            let parts = prefix
                .trim_start_matches('/')
                .split('/')
                .collect::<Vec<_>>();
            let ordinal = match parts.as_slice() {
                [collection @ ("items" | "output"), index] => {
                    let index = index.parse::<usize>().ok()?;
                    let count = value(&format!("/{collection}"))?.parse::<usize>().ok()?;
                    if index >= count {
                        return None;
                    }
                    index.checked_add(1)?
                }
                ["response", "output", index] => {
                    let index = index.parse::<usize>().ok()?;
                    let count = value("/response/output")?.parse::<usize>().ok()?;
                    if index >= count {
                        return None;
                    }
                    index.checked_add(1)?
                }
                ["item"] => 1,
                _ => return None,
            };
            (field.value == "message" && value(&format!("{prefix}/role")) == Some("assistant"))
                .then(|| (prefix.to_owned(), ordinal))
        })
        .collect::<Vec<_>>();
    let collections = messages
        .iter()
        .filter_map(|(prefix, _)| prefix.rsplit_once('/').map(|(collection, _)| collection))
        .map(|collection| {
            if collection.is_empty() {
                "/item"
            } else {
                collection
            }
        })
        .collect::<std::collections::BTreeSet<_>>();
    if messages.is_empty() || collections.len() > 1 {
        return fields.clone();
    }
    fields
        .iter()
        .filter_map(|field| {
            if (field.key == "/group_id" && field.value == "null")
                || (field.key == "/reasoning_epoch" && field.value == "0")
                || (matches!(
                    field.key.as_str(),
                    "/items" | "/output" | "/response/output"
                ) && field.value == "1"
                    && messages.len() == 1)
            {
                return None;
            }
            let mut field = field.clone();
            if matches!(
                field.key.as_str(),
                "/items" | "/output" | "/response/output"
            ) && field.value.parse::<usize>().ok() == Some(messages.len())
            {
                field.label = "回答数".into();
            }
            for (prefix, ordinal) in &messages {
                let Some(suffix) = field.key.strip_prefix(&format!("{prefix}/")) else {
                    continue;
                };
                let answer = if messages.len() > 1
                    || prefix
                        .rsplit_once('/')
                        .and_then(|(collection, _)| value(collection))
                        .is_some_and(|count| count != "1")
                {
                    format!("回答 {ordinal} › ")
                } else {
                    String::new()
                };
                if matches!(suffix, "role" | "type") {
                    return None;
                }
                if suffix == "status" {
                    field.label = format!("{answer}回答の状態");
                }
                if suffix == "content"
                    && field.value == "1"
                    && matches!(
                        value(&format!("{prefix}/content/0/type")),
                        Some("output_text" | "text")
                    )
                    && value(&format!("{prefix}/content/0/text")).is_some()
                {
                    return None;
                }
                if suffix == "content" {
                    field.label = format!("{answer}内容の数");
                }
                let parts = suffix.split('/').collect::<Vec<_>>();
                if let ["content", index, key] = parts.as_slice()
                    && let Ok(index) = index.parse::<usize>()
                    && value(&format!("{prefix}/content"))
                        .and_then(|count| count.parse::<usize>().ok())
                        .is_some_and(|count| index < count)
                {
                    let base = format!("{prefix}/content/{index}");
                    if matches!(value(&format!("{base}/type")), Some("output_text" | "text"))
                        && value(&format!("{base}/text")).is_some()
                    {
                        if *key == "type" {
                            return None;
                        }
                        if *key == "text" {
                            field.label = if value(&format!("{prefix}/content")) == Some("1") {
                                format!("{answer}本文")
                            } else {
                                format!("{answer}本文 {}", index + 1)
                            };
                        }
                    }
                }
            }
            Some(field)
        })
        .collect()
}

impl Details {
    pub fn new(view: &ThreadView, links: bool) -> Self {
        let selected_node = if view.follow {
            view.nodes().len().saturating_sub(1)
        } else {
            view.anchor
                .node_id
                .as_ref()
                .and_then(|id| view.nodes().iter().position(|node| &node.node_id == id))
                .unwrap_or(view.nodes().len().saturating_sub(1))
        };
        let node_id = view
            .nodes()
            .get(selected_node)
            .map(|node| node.node_id.clone())
            .unwrap_or_default();
        let revision = source_revision(view, &node_id);
        Self {
            source_node_ids: vec![node_id.clone()],
            item_key: None,
            item_history_revision: None,
            item_scoped: false,
            source_kind: view
                .nodes()
                .get(selected_node)
                .map(|node| node.presentation.kind.clone())
                .unwrap_or_default(),
            source_snapshot: None,
            sources_dirty: true,
            sources_stale: None,
            last_sources_request: None,
            selected_field_key: None,
            selected_link_url: None,
            links_pending: false,
            links_request_id: 0,
            links_error: None,
            node_id,
            mode: if links { Mode::Links } else { Mode::Nodes },
            selected: if links { 0 } else { selected_node },
            internal_fields: false,
            scroll: Cell::new(0),
            page: Cell::new(8),
            request_id: 0,
            original: None,
            original_observed_at: None,
            original_lines: Vec::new(),
            revision,
        }
    }
    pub(super) fn for_node(view: &ThreadView, node_id: String, links: bool) -> Self {
        let mut details = Self::new(view, links);
        details.source_kind = view
            .nodes()
            .iter()
            .find(|node| node.node_id == node_id)
            .map(|node| node.presentation.kind.clone())
            .unwrap_or_default();
        details.node_id = node_id.clone();
        details.source_node_ids = vec![node_id];
        details.item_scoped = true;
        details.mode = if links { Mode::Links } else { Mode::Fields };
        details.selected = 0;
        details.revision = source_revision(view, &details.node_id);
        details
    }
    pub(super) fn with_sources(mut self, sources: &[String]) -> Self {
        self.source_node_ids = sources.iter().take(2).cloned().collect();
        if !self.source_node_ids.contains(&self.node_id) {
            self.source_node_ids.truncate(1);
            self.source_node_ids.push(self.node_id.clone());
        }
        self
    }
    pub(super) fn with_item_key(mut self, key: super::ItemKey) -> Self {
        self.item_key = Some(key);
        self.item_history_revision = None;
        self
    }
    pub(super) fn reconcile_source_membership(&mut self, view: &ThreadView) -> bool {
        let revision = view.history_revision();
        if self.item_history_revision == Some(revision) {
            return false;
        }
        self.item_history_revision = Some(revision);
        let Some(super::ItemKey::Tool { group_id, call_id }) = &self.item_key else {
            return false;
        };
        // The active projection is finite. Include the two pinned source nodes
        // even if a newer tail moved them outside that projection.
        let (begin, end) = super::presentation::projection_bounds(view);
        let mut indices = (begin..end).collect::<std::collections::BTreeSet<_>>();
        for id in &self.source_node_ids {
            if let Some(index) = view.nodes().iter().position(|node| &node.node_id == id) {
                indices.insert(index);
            }
        }
        let mut call: Option<(u64, String)> = None;
        let mut result: Option<(u64, String)> = None;
        for index in indices {
            let node = &view.nodes()[index];
            for block in &node.presentation.blocks {
                let Some(tool) = &block.tool else {
                    continue;
                };
                if tool.group_id.as_ref() != Some(group_id) || &tool.call_id != call_id {
                    continue;
                }
                for (candidate, selected) in [
                    (&tool.call_node_id, &mut call),
                    (&tool.result_node_id, &mut result),
                ] {
                    if let Some(id) = candidate
                        && selected
                            .as_ref()
                            .is_none_or(|(read, _)| node.presentation.read_revision > *read)
                    {
                        *selected = Some((node.presentation.read_revision, id.clone()));
                    }
                }
            }
        }
        let mut ids = Vec::new();
        for (_, id) in call.into_iter().chain(result) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return false;
        }
        if !ids.contains(&self.node_id) && ids.len() < 2 {
            ids.insert(0, self.node_id.clone());
        }
        if ids == self.source_node_ids {
            return false;
        }
        self.source_node_ids = ids;
        self.invalidate_sources();
        true
    }
    fn links(&self, view: &ThreadView) -> Vec<crate::presentation::PresentationLink> {
        let mut links = Vec::new();
        for id in &self.source_node_ids {
            let observed = self
                .source_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.sources.iter().find(|source| &source.node_id == id));
            let source = if let Some(source) = observed {
                if source.unavailable_reason.is_some() {
                    continue;
                }
                source.links.as_slice()
            } else {
                view.nodes()
                    .iter()
                    .find(|node| &node.node_id == id)
                    .map_or(&[][..], |node| node.presentation.links.as_slice())
            };
            for link in source {
                if links.len() >= 128 {
                    return links;
                }
                if !links
                    .iter()
                    .any(|saved: &crate::presentation::PresentationLink| saved.url == link.url)
                {
                    links.push(link.clone());
                }
            }
        }
        links
    }
    pub(super) fn invalidate_sources(&mut self) {
        self.sources_dirty = true;
        self.sources_stale = Some("参照元の更新を確認中".into());
    }
    pub(super) fn mark_stale(&mut self, reason: &str) {
        self.sources_stale = Some(safe(reason));
    }
    pub(super) fn refresh_request(
        &mut self,
        thread_id: String,
        generation: u64,
        next: &mut u64,
        now: Instant,
    ) -> Option<UiAction> {
        if !self.sources_dirty
            || self.links_pending
            || self.last_sources_request.is_some_and(|last| {
                now.saturating_duration_since(last) < Duration::from_millis(500)
            })
        {
            return None;
        }
        Some(self.request_links_at(thread_id, generation, next, now))
    }
    pub(super) fn request_links(
        &mut self,
        thread_id: String,
        generation: u64,
        next: &mut u64,
    ) -> UiAction {
        self.request_links_at(thread_id, generation, next, Instant::now())
    }
    fn request_links_at(
        &mut self,
        thread_id: String,
        generation: u64,
        next: &mut u64,
        now: Instant,
    ) -> UiAction {
        *next = next.wrapping_add(1);
        self.links_request_id = *next;
        self.links_pending = true;
        self.sources_dirty = false;
        self.last_sources_request = Some(now);
        self.links_error = None;
        UiAction::LoadDetailLinks {
            thread_id,
            generation,
            request_id: self.links_request_id,
            node_ids: self.source_node_ids.clone(),
        }
    }
    pub(super) fn accept_links(
        &mut self,
        request_id: u64,
        result: Result<DetailSourceSnapshot, String>,
    ) {
        if self.links_request_id != request_id || !self.links_pending {
            return;
        }
        self.links_pending = false;
        if self.sources_dirty {
            return;
        }
        match result {
            Ok(mut snapshot) => {
                let expected = self
                    .source_node_ids
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>();
                let actual = snapshot
                    .sources
                    .iter()
                    .map(|source| &source.node_id)
                    .collect::<std::collections::BTreeSet<_>>();
                if snapshot.sources.len() > 2
                    || snapshot.sources.len() != expected.len()
                    || actual != expected
                {
                    self.links_error = Some("参照元が変わりました。開き直してください".into());
                    self.sources_stale = self.links_error.clone();
                    return;
                }
                if self
                    .source_snapshot
                    .as_ref()
                    .is_some_and(|old| snapshot.read_revision < old.read_revision)
                {
                    self.sources_dirty = true;
                    self.sources_stale = Some("古い参照元の応答を再取得中".into());
                    return;
                }
                for source in &mut snapshot.sources {
                    source.fields.truncate(128);
                    source.links.truncate(128);
                    if source.unavailable_reason.is_some() {
                        source.links.clear();
                    }
                }
                self.source_snapshot = Some(snapshot);
                self.sources_stale = None;
                self.links_error = None;
                if matches!(self.mode, Mode::Links)
                    && let Some(url) = &self.selected_link_url
                {
                    let mut links = Vec::new();
                    for source in &self.source_snapshot.as_ref().unwrap().sources {
                        if source.unavailable_reason.is_none() {
                            for link in &source.links {
                                if !links.iter().any(
                                    |saved: &&crate::presentation::PresentationLink| {
                                        saved.url == link.url
                                    },
                                ) {
                                    links.push(link);
                                }
                            }
                        }
                    }
                    self.selected = links
                        .iter()
                        .position(|link| &link.url == url)
                        .unwrap_or_else(|| self.selected.min(links.len().saturating_sub(1)));
                }
                if matches!(self.mode, Mode::Fields)
                    && let Some(key) = &self.selected_field_key
                    && let Some(fields) = self.snapshot_fields()
                    && let Some(index) = fields.iter().position(|field| &field.key == key)
                {
                    self.selected = index;
                }
            }
            Err(error) => {
                self.links_error = Some(safe(&error));
                self.sources_stale = Some("取得失敗 · 前回の観測値を表示".into());
            }
        }
    }
    fn snapshot_fields(&self) -> Option<Vec<PresentationField>> {
        let snapshot = self.source_snapshot.as_ref()?;
        let source = snapshot
            .sources
            .iter()
            .find(|source| source.node_id == self.node_id)?;
        let mut fields = vec![PresentationField {
            key: "node_id".into(),
            label: "履歴node ID".into(),
            value: self.node_id.clone(),
            display: None,
            value_omitted: false,
        }];
        if let Some(reason) = &source.unavailable_reason {
            fields.push(PresentationField {
                key: "/ui/source_unavailable".into(),
                label: "参照元を取得できません".into(),
                value: reason.clone(),
                display: None,
                value_omitted: false,
            });
        } else {
            let node = UiNode {
                presentation: crate::presentation::NodePresentation {
                    kind: if source.kind == PresentationKind::Unknown {
                        self.source_kind.clone()
                    } else {
                        source.kind.clone()
                    },
                    fields: source.fields.clone(),
                    ..Default::default()
                },
                ..Default::default()
            };
            fields.extend(if self.internal_fields {
                source.fields.clone()
            } else {
                conversation_fields(&node)
            });
        }
        fields.sort_by_key(|field| {
            (
                !field.key.starts_with("/current_tools/"),
                crate::presentation::fields::group(&field.key).0,
            )
        });
        Some(fields)
    }
    pub fn accept(&mut self, request_id: u64, body: HistoryBody, display: Vec<Line<'static>>) {
        if request_id == self.request_id && body.node_id == self.node_id {
            self.original = Some(body);
            self.original_observed_at =
                Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            self.original_lines = display;
            self.scroll.set(0);
        }
    }
    fn fields(&self, view: &ThreadView) -> Vec<PresentationField> {
        if let Some(fields) = self.snapshot_fields() {
            return fields;
        }
        let mut fields = vec![PresentationField {
            value_omitted: false,
            display: None,
            key: "node_id".into(),
            label: "履歴node ID".into(),
            value: self.node_id.clone(),
        }];
        if let Some(node) = view.nodes().iter().find(|n| n.node_id == self.node_id) {
            fields.extend(if self.internal_fields {
                node.presentation.fields.clone()
            } else {
                conversation_fields(node)
            });
        }
        fields.sort_by_key(|field| crate::presentation::fields::group(&field.key).0);
        fields
    }
    pub(super) fn open_source(&mut self, node_id: String, next: &mut u64) -> UiAction {
        self.change_source(node_id);
        self.original(None, next)
    }
    fn change_source(&mut self, node_id: String) {
        if self.node_id == node_id {
            return;
        }
        self.original = None;
        self.original_lines.clear();
        self.original_observed_at = None;
        self.request_id = 0;
        self.source_kind = PresentationKind::Unknown;
        if !self.source_node_ids.contains(&node_id) {
            self.item_key = None;
            self.item_history_revision = None;
            self.source_node_ids = vec![node_id.clone()];
            self.source_snapshot = None;
            self.sources_dirty = true;
            self.sources_stale = Some("参照元の変更を確認中".into());
            self.links_error = None;
            self.selected_field_key = None;
            self.selected_link_url = None;
            if !self.links_pending {
                self.links_request_id = 0;
            }
        }
        self.node_id = node_id;
    }
    fn original(&mut self, cursor: Option<String>, next: &mut u64) -> UiAction {
        *next = next.wrapping_add(1);
        self.request_id = *next;
        self.mode = Mode::Original;
        UiAction::LoadOriginal {
            node_id: self.node_id.clone(),
            cursor,
            request_id: self.request_id,
        }
    }
    pub fn key(&mut self, key: KeyEvent, view: &ThreadView, next: &mut u64) -> Outcome {
        self.sync_node(view);
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Char('r') if !matches!(self.mode, Mode::Original) => {
                self.invalidate_sources();
                if !self.links_pending
                    && let Some(thread) = view.thread_id.clone()
                {
                    return Outcome::Action(Box::new(self.request_links(
                        thread,
                        view.generation,
                        next,
                    )));
                }
            }
            KeyCode::F(6) => {
                self.mode = if self.item_scoped {
                    Mode::Fields
                } else {
                    Mode::Nodes
                };
                self.selected = if self.item_scoped {
                    0
                } else {
                    view.nodes()
                        .iter()
                        .position(|node| node.node_id == self.node_id)
                        .unwrap_or(0)
                };
                self.scroll.set(0);
            }
            KeyCode::F(7) | KeyCode::Char('l') => {
                self.mode = Mode::Links;
                self.selected = 0;
                self.scroll.set(0);
            }
            KeyCode::Char('o') => return Outcome::Action(Box::new(self.original(None, next))),
            KeyCode::Char('n') if matches!(self.mode, Mode::Original) => {
                if let Some(cursor) = self.original.as_ref().and_then(|b| b.next_cursor.clone()) {
                    return Outcome::Action(Box::new(self.original(Some(cursor), next)));
                }
            }
            KeyCode::Char('p') if matches!(self.mode, Mode::Original) => {
                return Outcome::Action(Box::new(self.original(None, next)));
            }
            KeyCode::PageDown if matches!(self.mode, Mode::Original) => self.scroll.set(
                self.scroll
                    .get()
                    .saturating_add(self.page.get().min(u16::MAX as usize) as u16),
            ),
            KeyCode::PageUp if matches!(self.mode, Mode::Original) => self.scroll.set(
                self.scroll
                    .get()
                    .saturating_sub(self.page.get().min(u16::MAX as usize) as u16),
            ),
            KeyCode::Down if matches!(self.mode, Mode::Original) => {
                self.scroll.set(self.scroll.get().saturating_add(1))
            }
            KeyCode::Up if matches!(self.mode, Mode::Original) => {
                self.scroll.set(self.scroll.get().saturating_sub(1))
            }
            KeyCode::PageDown => self.selected = self.selected.saturating_add(self.page.get()),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(self.page.get()),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = self.selected.saturating_add(1),
            KeyCode::Char('v' | 'V') if matches!(self.mode, Mode::Fields) => {
                self.internal_fields = !self.internal_fields;
                self.selected = 0;
                self.scroll.set(0);
            }
            KeyCode::Char('a') => return Outcome::Operation(super::operations::OpKind::HistoryGet),
            KeyCode::Char('c') => {
                let text = match self.mode {
                    Mode::Nodes => Some(self.node_id.clone()),
                    Mode::Fields => {
                        let field = self.fields(view).get(self.selected).cloned();
                        if field.as_ref().is_some_and(|f| f.value_omitted) {
                            return Outcome::Action(Box::new(self.original(None, next)));
                        }
                        field.map(|f| f.value)
                    }
                    Mode::Links => self
                        .links(view)
                        .get(self.selected)
                        .map(|link| link.url.clone()),
                    Mode::Original => self.original.as_ref().map(|b| b.text.clone()),
                };
                if let Some(text) = text {
                    let scope = if matches!(self.mode, Mode::Original) {
                        super::CopyScope::OriginalPage {
                            has_more: self
                                .original
                                .as_ref()
                                .is_some_and(|body| body.next_cursor.is_some()),
                        }
                    } else {
                        super::CopyScope::Value
                    };
                    return Outcome::Action(Box::new(UiAction::CopyText { text, scope }));
                }
            }
            KeyCode::Enter => match self.mode {
                Mode::Nodes => {
                    if let Some(node) = view
                        .nodes()
                        .get(self.selected.min(view.nodes().len().saturating_sub(1)))
                    {
                        self.node_id = node.node_id.clone();
                        self.revision = source_revision(view, &self.node_id);
                        self.mode = Mode::Fields;
                        self.internal_fields = false;
                        self.selected = 0;
                        self.scroll.set(0);
                        self.original = None;
                        self.original_lines = Vec::new();
                    }
                }
                Mode::Links => {}
                Mode::Fields => {
                    if let Some(field) = self.fields(view).get(self.selected) {
                        let key = field.key.rsplit(['/', '.']).next().unwrap_or(&field.key);
                        let operation = super::operations::TargetKind::from_field_key(&field.key)
                            .and_then(super::operations::TargetKind::detail_operation);
                        if let Some(operation) = operation {
                            return Outcome::Operation(operation);
                        }
                        if key.ends_with("node_id") {
                            return Outcome::Action(Box::new(
                                self.open_source(field.value.clone(), next),
                            ));
                        }
                    }
                }
                Mode::Original => {}
            },
            _ => {}
        }
        let count = match self.mode {
            Mode::Nodes => view.nodes().len(),
            Mode::Fields => self.fields(view).len(),
            Mode::Links => self.links(view).len(),
            Mode::Original => 1,
        };
        self.selected = self.selected.min(count.saturating_sub(1));
        if matches!(self.mode, Mode::Fields) {
            self.selected_field_key = self
                .fields(view)
                .get(self.selected)
                .map(|field| field.key.clone());
        }
        if matches!(self.mode, Mode::Links) {
            self.selected_link_url = self
                .links(view)
                .get(self.selected)
                .map(|link| link.url.clone());
        }
        self.sync_node(view);
        Outcome::None
    }
    fn sync_node(&mut self, view: &ThreadView) {
        if matches!(self.mode, Mode::Nodes)
            && let Some(node) = view
                .nodes()
                .get(self.selected.min(view.nodes().len().saturating_sub(1)))
            && self.node_id != node.node_id
        {
            self.change_source(node.node_id.clone());
            self.source_kind = node.presentation.kind.clone();
            self.revision = source_revision(view, &self.node_id);
            self.original = None;
            self.original_lines = Vec::new();
        }
    }
    pub fn selected_field(&self, view: &ThreadView) -> Option<PresentationField> {
        if matches!(self.mode, Mode::Fields) {
            self.fields(view).get(self.selected).cloned()
        } else {
            None
        }
    }
    pub fn draw(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        view: &ThreadView,
    ) -> Vec<crate::terminal_links::LinkRegion> {
        let count = match self.mode {
            Mode::Nodes => view.nodes().len(),
            Mode::Fields => self.fields(view).len(),
            Mode::Links => self.links(view).len(),
            Mode::Original => 0,
        };
        let mode = match self.mode {
            Mode::Nodes => "会話の詳細",
            Mode::Fields if self.internal_fields => "保持中の内部項目",
            Mode::Fields => "選択した内容の詳細",
            Mode::Links => "リンク",
            Mode::Original => "原文",
        };
        let context = if matches!(self.mode, Mode::Nodes) {
            format!(
                "{} / {}件 · 内容を選んでEnterで詳細を表示",
                self.selected.saturating_add(1).min(count),
                count
            )
        } else if matches!(self.mode, Mode::Fields) {
            view.nodes()
                .iter()
                .find(|node| node.node_id == self.node_id)
                .map(|node| {
                    let (label, preview) = entry(node, usize::from(area.width.saturating_sub(16)));
                    format!("{label} · {preview}")
                })
                .unwrap_or_else(|| format!("node {}", safe(&self.node_id)))
        } else if matches!(self.mode, Mode::Original) {
            self.original
                .as_ref()
                .map(|body| {
                    format!(
                        "保存 {} · 表示受信 {}",
                        safe(&body.provenance.created_at),
                        self.original_observed_at.as_deref().unwrap_or("未確認")
                    )
                })
                .unwrap_or_else(|| format!("node {}", safe(&self.node_id)))
        } else {
            format!(
                "{} / {}件 · node {}",
                self.selected.saturating_add(1).min(count),
                count,
                safe(&self.node_id)
            )
        };
        let footer = if matches!(self.mode, Mode::Original) {
            "Esc 戻る · ↑↓/PgUp/PgDn スクロール\nN 次page · P 先頭再取得 · O 再取得 · C 原文copy · F6 項目 · F7 リンク"
        } else if matches!(self.mode, Mode::Fields) {
            "Esc 戻る · ↑↓/PgUp/PgDn 選択 · Enter 詳細\nR 再取得 · O 原文 · C 値copy · V 要点/内部項目 · A 操作"
        } else {
            "Esc 戻る · ↑↓/PgUp/PgDn 選択 · Enter 詳細\nR 再取得 · O 原文 · C 値copy · A 操作 · F6 項目 · F7 リンク"
        };
        let mut body = popup::panel(frame, area, mode, &context, footer);
        if self.revision != source_revision(view, &self.node_id) && body.height > 1 {
            frame.render_widget(
                ratatui::widgets::Paragraph::new("表示が更新されました。node参照を維持しています")
                    .style(Style::default().fg(theme::WARNING)),
                Rect { height: 1, ..body },
            );
            body.y += 1;
            body.height -= 1;
        }
        if body.height > 1
            && (self.links_pending
                || self.sources_stale.is_some()
                || self.links_error.is_some()
                || self.source_snapshot.is_some())
        {
            let mut parts = Vec::new();
            if self.links_pending {
                parts.push("参照元を取得中".to_owned());
            }
            if let Some(stale) = &self.sources_stale {
                parts.push(stale.clone());
            }
            if let Some(error) = &self.links_error {
                parts.push(error.clone());
            }
            if let Some(snapshot) = &self.source_snapshot {
                if snapshot
                    .sources
                    .iter()
                    .any(|source| source.unavailable_reason.is_some())
                {
                    parts.push("参照元が利用不可 · 原文の表示済みコピーとは別".into());
                }
                parts.push(format!("観測 {} · R再取得", snapshot.observed_at));
            }
            frame.render_widget(
                ratatui::widgets::Paragraph::new(safe(&parts.join(" · ")))
                    .style(Style::default().fg(theme::WARNING)),
                Rect { height: 1, ..body },
            );
            body.y += 1;
            body.height -= 1;
        }
        let metadata_notice = view
            .nodes()
            .iter()
            .find(|node| node.node_id == self.node_id)
            .and_then(|node| {
                let p = &node.presentation;
                if p.metadata_limited {
                    Some(format!(
                        "詳細{}項目を保持・上限あり · A 対象を照会",
                        p.fields.len()
                    ))
                } else if p.details_omitted && p.body_omitted_chars == 0 && !p.body_extent_unknown {
                    Some("表示情報の保持範囲は未確認 · A 対象を照会".into())
                } else {
                    None
                }
            });
        if matches!(self.mode, Mode::Fields)
            && body.height > 1
            && let Some(notice) = metadata_notice
        {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(notice).style(Style::default().fg(theme::WARNING)),
                Rect { height: 1, ..body },
            );
            body.y += 1;
            body.height -= 1;
        }
        self.page
            .set(usize::from(body.height).saturating_sub(1).max(1));
        let mut targets = Vec::new();
        let mut field_selection = None;
        let items = match self.mode {
            Mode::Nodes => view
                .nodes()
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    let (label, preview) = entry(node, usize::from(body.width.saturating_sub(5)));
                    popup::item(&format!("{} · {label}\n  {preview}", index + 1), body.width)
                })
                .collect(),
            Mode::Fields => {
                let mut rows = Vec::new();
                let mut previous = None;
                let mut selected_row = 0;
                for (index, field) in self.fields(view).iter().enumerate() {
                    let group = crate::presentation::fields::group(&field.key);
                    if previous != Some(group.0) {
                        rows.push(popup::Item::new(
                            Line::from(format!("── {} ──", group.1))
                                .style(Style::default().fg(theme::GROUP_HEADING)),
                        ));
                        previous = Some(group.0);
                    }
                    if index == self.selected {
                        selected_row = rows.len();
                    }
                    rows.push(
                        popup::item(
                            &format!("{}: {}", safe(&field.label), safe(field.displayed())),
                            body.width,
                        )
                        .preview(4),
                    );
                }
                field_selection = Some(selected_row);
                rows
            }
            Mode::Links => self
                .links(view)
                .iter()
                .enumerate()
                .map(|(index, link)| {
                    targets.push(std::sync::Arc::<str>::from(link.url.as_str()));
                    let mut lines = Vec::new();
                    if link.label != link.url {
                        lines.push(Line::from(safe(&link.label)));
                    }
                    let style = Style::default()
                        .fg(theme::LINK)
                        .underline_color(crate::terminal_links::marker_color(index));
                    lines.push(Line::from(Span::styled(safe(&link.url), style)));
                    ListItem::new(super::presentation::hanging_wrap(
                        lines,
                        body.width.saturating_sub(3).max(1),
                    ))
                })
                .collect(),
            Mode::Original => {
                let loading = [Line::from("原文を取得中…")];
                let lines = if self.original.is_some() {
                    self.original_lines.as_slice()
                } else {
                    &loading
                };
                self.scroll.set(popup::reader_prepared(
                    frame,
                    body,
                    super::presentation::borrowed(lines),
                    self.scroll.get(),
                ));
                return vec![];
            }
        };
        if count == 0 {
            let message = if matches!(self.mode, Mode::Links) {
                "このwindowにHTTP(S)リンクはありません。O原文から別pageも確認できます"
            } else {
                "表示できる項目はありません"
            };
            popup::reader(frame, body, &[Line::from(message)], 0);
        } else {
            popup::list(frame, body, items, field_selection.unwrap_or(self.selected));
        }
        // Read actual List cells so variable-height items, its offset, and
        // partial visibility share exactly the same URL hit-test geometry.
        let buffer = frame.buffer_mut();
        let regions = crate::terminal_links::collect_regions(
            buffer,
            Rect::new(
                body.x.saturating_add(2).min(body.right()),
                body.y,
                body.width.saturating_sub(3),
                body.height,
            ),
            &targets,
            |cell| cell.underline_color,
        );
        // Marker colors are private layout data, never terminal styling.
        for region in &regions {
            for x in region.x..region.x + region.width {
                buffer[(x, region.y)].underline_color = Color::Reset;
            }
        }
        regions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        presentation::{NodePresentation, PresentationLink},
        ui::{UiNode, UiState},
    };
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};
    fn state() -> UiState {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("anonymous".into());
        *ui.view_mut().nodes_mut_for_test() = vec![UiNode {
            node_id: "source-node".into(),
            role: "assistant".into(),
            text: "saved".into(),
            base_char_offset: 0,
            total_chars: 5,
            reasoning_ranges: vec![],
            presentation: NodePresentation {
                links: (0..30)
                    .map(|index| PresentationLink {
                        label: format!("リンク{index:02}"),
                        url: format!(
                            "https://example.invalid/full/path/{index:02}?keep=日本語&all=yes"
                        ),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
        }];
        ui
    }
    fn screen(terminal: &Terminal<TestBackend>) -> String {
        use unicode_width::UnicodeWidthStr;
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut text = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    text.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                text
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn rejected_link_action_exits_loading_with_an_actionable_error() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
        ui.select_thread("t".into());
        let mut details = Details::for_node(ui.view(), "source".into(), true);
        let mut next = 0;
        let action = details.request_links("t".into(), ui.view().generation, &mut next);
        ui.details = Some(details);
        ui.action_rejected(&action);
        let details = ui.details.as_ref().unwrap();
        assert!(!details.links_pending);
        assert!(details.links_error.as_ref().unwrap().contains("再試行"));
    }
    fn snapshot_for(
        details: &Details,
        view: &ThreadView,
        extra: Vec<PresentationLink>,
        read_revision: u64,
    ) -> DetailSourceSnapshot {
        DetailSourceSnapshot {
            read_revision,
            observed_at: "2026-10-30T12:34:56Z".into(),
            sources: details
                .source_node_ids
                .iter()
                .map(|id| {
                    let node = view.nodes().iter().find(|node| &node.node_id == id);
                    let mut links =
                        node.map_or_else(Vec::new, |node| node.presentation.links.clone());
                    if id == &details.node_id {
                        links.extend(extra.clone());
                    }
                    DetailSourceFacts {
                        node_id: id.clone(),
                        kind: node.map_or(PresentationKind::Unknown, |node| {
                            node.presentation.kind.clone()
                        }),
                        links,
                        fields: node.map_or_else(Vec::new, |node| node.presentation.fields.clone()),
                        unavailable_reason: None,
                    }
                })
                .collect(),
        }
    }
    #[test]
    fn refreshed_selection_uses_only_the_current_mode_identity() {
        let ui = state();
        let mut details = Details::for_node(ui.view(), "source-node".into(), false);
        let mut next = 0;
        let snapshot = || DetailSourceSnapshot {
            read_revision: 1,
            observed_at: "UTC".into(),
            sources: vec![DetailSourceFacts {
                node_id: "source-node".into(),
                fields: vec![PresentationField {
                    key: "special".into(),
                    label: "saved field".into(),
                    value: "value".into(),
                    ..Default::default()
                }],
                links: ["https://example.invalid/a", "https://example.invalid/b"]
                    .into_iter()
                    .map(|url| PresentationLink {
                        url: url.into(),
                        label: url.into(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
        };
        details.request_links("anonymous".into(), ui.view().generation, &mut next);
        details.accept_links(next, Ok(snapshot()));
        details.key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert_eq!(details.selected_field_key.as_deref(), Some("node_id"));
        details.key(
            KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        details.key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert_eq!(details.selected, 1);
        details.request_links("anonymous".into(), ui.view().generation, &mut next);
        details.accept_links(next, Ok(snapshot()));
        assert_eq!(
            details.selected, 1,
            "old field key must not override current URL selection"
        );
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'),KeyModifiers::NONE),ui.view(),&mut next),Outcome::Action(action) if matches!(&*action,UiAction::CopyText {text,..} if text=="https://example.invalid/b"))
        );
    }

    #[test]
    fn manual_refresh_during_pending_read_retires_before_one_coalesced_followup() {
        let ui = state();
        let mut details = Details::for_node(ui.view(), "source-node".into(), true);
        let mut next = 0;
        let now = Instant::now();
        let first = details
            .refresh_request("anonymous".into(), ui.view().generation, &mut next, now)
            .unwrap();
        let token = match first {
            UiAction::LoadDetailLinks { request_id, .. } => request_id,
            _ => unreachable!(),
        };
        assert!(matches!(
            details.key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                ui.view(),
                &mut next
            ),
            Outcome::None
        ));
        assert!(details.sources_dirty);
        assert!(
            details
                .refresh_request(
                    "anonymous".into(),
                    ui.view().generation,
                    &mut next,
                    now + Duration::from_secs(1)
                )
                .is_none(),
            "physical read must retire first"
        );
        let old = snapshot_for(&details, ui.view(), vec![], 1);
        details.accept_links(token, Ok(old));
        assert!(details.source_snapshot.is_none() && !details.links_pending);
        assert!(
            details
                .refresh_request(
                    "anonymous".into(),
                    ui.view().generation,
                    &mut next,
                    now + Duration::from_millis(499)
                )
                .is_none()
        );
        assert!(
            details
                .refresh_request(
                    "anonymous".into(),
                    ui.view().generation,
                    &mut next,
                    now + Duration::from_millis(500)
                )
                .is_some()
        );
        assert!(next > token);
    }

    #[test]
    fn scoped_offwindow_links_append_without_inserting_source_nodes_and_reject_old_replies() {
        let ui = state();
        let before = ui.view().nodes().to_vec();
        let ids = vec!["source-node".to_owned(), "offwindow-result".to_owned()];
        let mut details =
            Details::for_node(ui.view(), "offwindow-result".into(), true).with_sources(&ids);
        let mut next = 10;
        let request = details.request_links("anonymous".into(), ui.view().generation, &mut next);
        let token = match request {
            UiAction::LoadDetailLinks {
                request_id,
                node_ids,
                ..
            } => {
                assert_eq!(node_ids, ids);
                request_id
            }
            _ => panic!("expected scoped links read"),
        };
        let extra = PresentationLink {
            label: "saved result link".into(),
            url: "https://example.invalid/offwindow".into(),
            ..Default::default()
        };
        details.accept_links(
            token - 1,
            Ok(snapshot_for(&details, ui.view(), vec![extra.clone()], 1)),
        );
        assert!(details.links_pending && details.source_snapshot.is_none());
        details.accept_links(
            token,
            Ok(snapshot_for(&details, ui.view(), vec![extra.clone()], 1)),
        );
        assert!(!details.links_pending);
        let links = details.links(ui.view());
        assert_eq!(links.len(), 31);
        assert!(links.iter().any(|link| link.url == extra.url));
        assert_eq!(details.node_id, "offwindow-result");
        assert_eq!(
            ui.view().nodes(),
            &before,
            "offpage link metadata must not alter history order/body windows"
        );
        details.accept_links(token, Err("late duplicate".into()));
        assert!(details.links_error.is_none());
    }
    #[test]
    fn original_page_requests_do_not_invalidate_separate_scoped_link_request() {
        let ui = state();
        let mut details = Details::for_node(ui.view(), "source-node".into(), true);
        let mut next = 0;
        details.request_links("anonymous".into(), ui.view().generation, &mut next);
        let links_token = details.links_request_id;
        let original = details.open_source("source-node".into(), &mut next);
        assert!(
            matches!(original,UiAction::LoadOriginal{request_id,..} if request_id!=links_token)
        );
        details.accept_links(
            links_token,
            Ok(snapshot_for(
                &details,
                ui.view(),
                vec![PresentationLink {
                    label: "later".into(),
                    url: "https://example.invalid/later".into(),
                    ..Default::default()
                }],
                1,
            )),
        );
        assert!(
            details
                .links(ui.view())
                .iter()
                .any(|link| link.url.ends_with("/later"))
        );
        assert!(!details.links_pending);
        assert!(matches!(details.mode, Mode::Original));
        assert_ne!(details.request_id, details.links_request_id);
    }
    #[test]
    fn selecting_another_source_clears_scoped_link_cache_and_invalidates_pending_reply() {
        let mut ui = state();
        let mut second = ui.view().nodes()[0].clone();
        second.node_id = "second".into();
        second.presentation.links.clear();
        ui.view_mut().nodes_mut_for_test().push(second);
        let mut details = Details::new(ui.view(), false);
        details.selected = 0;
        details.sync_node(ui.view());
        let mut next = 20;
        details.request_links("anonymous".into(), ui.view().generation, &mut next);
        let old = details.links_request_id;
        details.accept_links(
            old,
            Ok(snapshot_for(
                &details,
                ui.view(),
                vec![PresentationLink {
                    url: "https://example.invalid/old".into(),
                    ..Default::default()
                }],
                1,
            )),
        );
        details.request_links("anonymous".into(), ui.view().generation, &mut next);
        let pending = details.links_request_id;
        details.selected = 1;
        details.sync_node(ui.view());
        assert_eq!(details.node_id, "second");
        assert!(details.source_snapshot.is_none());
        assert!(
            details.links_pending,
            "scope changes wait for the physical old reply before another read"
        );
        details.accept_links(
            pending,
            Ok(DetailSourceSnapshot {
                read_revision: 2,
                observed_at: "old".into(),
                sources: vec![DetailSourceFacts {
                    node_id: "source-node".into(),
                    links: vec![PresentationLink {
                        url: "https://example.invalid/stale".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }),
        );
        assert!(details.links(ui.view()).is_empty());
    }
    #[test]
    fn accepted_thread_changes_refresh_visible_facts_and_gc_overrides_cached_links() {
        let mut ui = state();
        let generation = ui.view().generation;
        let base = Instant::now();
        ui.take_deferred_actions();
        ui.details = Some(Details::for_node(ui.view(), "source-node".into(), false));
        let request = |ui: &mut UiState, now| {
            ui.take_deferred_actions_at(now)
                .into_iter()
                .find_map(|action| match action {
                    UiAction::LoadDetailLinks { request_id, .. } => Some(request_id),
                    _ => None,
                })
                .unwrap()
        };
        let snapshot = |revision, state: &str, unavailable| DetailSourceSnapshot {
            read_revision: revision,
            observed_at: format!("2026-10-30T12:34:{revision:02}Z"),
            sources: vec![DetailSourceFacts {
                node_id: "source-node".into(),
                fields: vec![PresentationField {
                    key: "/current_tools/0/state".into(),
                    label: "現在の状態".into(),
                    value: state.into(),
                    ..Default::default()
                }],
                links: vec![PresentationLink {
                    url: "https://example.invalid/fresh".into(),
                    label: "fresh".into(),
                    ..Default::default()
                }],
                unavailable_reason: unavailable,
                ..Default::default()
            }],
        };
        let first = request(&mut ui, base);
        ui.apply(super::super::UiUpdate::DetailLinks {
            thread_id: "anonymous".into(),
            generation,
            request_id: first,
            result: Ok(snapshot(1, "running", None)),
        });
        assert!(
            ui.details
                .as_ref()
                .unwrap()
                .fields(ui.view())
                .iter()
                .any(|field| field.value == "running")
        );
        let changed = || {
            serde_json::from_value(serde_json::json!({"thread_id":"anonymous","title":"changed","cwd":"/synthetic","connection":"fixture","model":"fixture","agents_path":"","children_count":0,"shells_count":0})).unwrap()
        };
        ui.apply(super::super::UiUpdate::Thread {
            generation,
            details: changed(),
            draft: None,
        });
        assert!(
            !ui.take_deferred_actions_at(base + Duration::from_millis(499))
                .iter()
                .any(|action| matches!(action, UiAction::LoadDetailLinks { .. }))
        );
        let second = request(&mut ui, base + Duration::from_millis(500));
        ui.apply(super::super::UiUpdate::DetailLinks {
            thread_id: "anonymous".into(),
            generation,
            request_id: second,
            result: Ok(snapshot(2, "exited", None)),
        });
        let mut terminal = Terminal::new(TestBackend::new(90, 28)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &mut ui))
            .unwrap();
        assert!(screen(&terminal).contains("exited"));
        ui.apply(super::super::UiUpdate::Thread {
            generation,
            details: changed(),
            draft: None,
        });
        let third = request(&mut ui, base + Duration::from_millis(1000));
        ui.apply(super::super::UiUpdate::DetailLinks {
            thread_id: "anonymous".into(),
            generation,
            request_id: third,
            result: Ok(snapshot(3, "exited", Some("source collected".into()))),
        });
        assert_eq!(
            ui.view().nodes()[0].presentation.links.len(),
            30,
            "underlying cached source remains intentionally untouched"
        );
        assert!(
            ui.details.as_ref().unwrap().links(ui.view()).is_empty(),
            "fresh unavailability overrides every stale loaded URL"
        );
        ui.details.as_mut().unwrap().mode = Mode::Links;
        terminal
            .draw(|frame| super::super::draw(frame, &mut ui))
            .unwrap();
        let shown = screen(&terminal);
        assert!(shown.contains("利用不可"));
        assert!(!shown.contains("example.invalid"));
    }
    #[test]
    fn tool_metadata_membership_growth_refreshes_open_popup_without_retargeting_original() {
        use crate::presentation::{BlockKind, PresentationBlock, ToolPresentation};
        let mut ui = state();
        let generation = ui.view().generation;
        let base = Instant::now();
        let tool = ToolPresentation {
            group_id: Some("saved-group".into()),
            call_id: "saved-call".into(),
            call_node_id: Some("source-node".into()),
            ..Default::default()
        };
        ui.view_mut().nodes_mut_for_test()[0].presentation.blocks = vec![PresentationBlock {
            kind: BlockKind::ToolCall,
            tool: Some(tool),
            ..Default::default()
        }];
        let key = super::super::ItemKey::Tool {
            group_id: "saved-group".into(),
            call_id: "saved-call".into(),
        };
        ui.take_deferred_actions();
        let mut details = Details::for_node(ui.view(), "source-node".into(), true)
            .with_sources(&["source-node".into()])
            .with_item_key(key);
        details.mode = Mode::Original;
        details.request_id = 41;
        let body:HistoryBody=serde_json::from_value(serde_json::json!({"node_id":"source-node","kind":"assistant_response","text":"PAID ORIGINAL","context":{"state":"present","node_id":"source-node","created_at":"2026-10-30T00:00:00Z"},"provenance":{"kind":"assistant_response","created_at":"2026-10-30T00:00:00Z","source_revision":0},"source_node_ids":[],"previous_node_ids":[],"next_node_ids":[]})).unwrap();
        details.accept(41, body, vec![Line::from("PAID ORIGINAL")]);
        let observed = details.original_observed_at.clone();
        ui.details = Some(details);
        ui.next_detail_request = 41;
        let source_request = |ui: &mut UiState, now| {
            ui.take_deferred_actions_at(now)
                .into_iter()
                .find_map(|action| match action {
                    UiAction::LoadDetailLinks {
                        request_id,
                        node_ids,
                        ..
                    } => Some((request_id, node_ids)),
                    _ => None,
                })
        };
        let (old, ids) = source_request(&mut ui, base).unwrap();
        assert_eq!(ids, ["source-node"]);
        let old_snapshot = snapshot_for(ui.details.as_ref().unwrap(), ui.view(), vec![], 10);
        let before = ui.view().history_revision();
        let mut fresh = ui.view().nodes()[0].clone();
        fresh.presentation.blocks[0]
            .tool
            .as_mut()
            .unwrap()
            .result_node_id = Some("later-result".into());
        ui.view_mut().tool_refresh = Some(before);
        ui.apply(super::super::UiUpdate::ToolFacts {
            thread_id: "anonymous".into(),
            generation,
            revision: before,
            result: Ok(vec![fresh]),
        });
        assert!(
            ui.view().history_revision() > before,
            "tool-reference-only updates must advance source revision"
        );
        assert!(
            source_request(&mut ui, base + Duration::from_millis(501)).is_none(),
            "a changed membership cannot overlap its prior physical read"
        );
        assert_eq!(
            ui.details.as_ref().unwrap().source_node_ids,
            ["source-node", "later-result"]
        );
        ui.apply(super::super::UiUpdate::DetailLinks {
            thread_id: "anonymous".into(),
            generation,
            request_id: old,
            result: Ok(old_snapshot),
        });
        assert!(ui.details.as_ref().unwrap().source_snapshot.is_none());
        let (next, ids) = source_request(&mut ui, base + Duration::from_millis(501)).unwrap();
        assert_eq!(ids, ["source-node", "later-result"]);
        let result_link = PresentationLink {
            url: "https://example.invalid/new-result".into(),
            ..Default::default()
        };
        ui.apply(super::super::UiUpdate::DetailLinks {
            thread_id: "anonymous".into(),
            generation,
            request_id: next,
            result: Ok(DetailSourceSnapshot {
                read_revision: 11,
                observed_at: "2026-10-30T12:34:56Z".into(),
                sources: vec![
                    DetailSourceFacts {
                        node_id: "source-node".into(),
                        ..Default::default()
                    },
                    DetailSourceFacts {
                        node_id: "later-result".into(),
                        kind: PresentationKind::ToolResult,
                        links: vec![result_link.clone()],
                        ..Default::default()
                    },
                ],
            }),
        });
        let details = ui.details.as_ref().unwrap();
        assert_eq!(details.node_id, "source-node");
        assert_eq!(details.request_id, 41);
        assert_eq!(details.original.as_ref().unwrap().text, "PAID ORIGINAL");
        assert_eq!(details.original_observed_at, observed);
        assert!(
            details
                .links(ui.view())
                .iter()
                .any(|link| link.url == result_link.url)
        );
        ui.details
            .as_mut()
            .unwrap()
            .open_source("unrelated".into(), &mut 100);
        assert!(ui.details.as_ref().unwrap().item_key.is_none());
    }
    #[test]
    fn saved_interruption_sections_are_distinguishable_and_keep_their_original_targets() {
        let mut ui = state();
        let template = ui.view().nodes()[0].clone();
        ui.view_mut().nodes_mut_for_test().clear();
        for (section, label) in [
            ("runs", "作業"),
            ("turns", "応答処理"),
            ("shells", "コマンド"),
            ("owned_children", "子の作業"),
        ] {
            let payload = serde_json::json!({"text":serde_json::json!({"section":section,"items":[{"execution_id":"complete-saved-id","previous_state":"running","planned_state":"stopped","result_uncertain":true}]}).to_string()});
            let text = crate::history::text_of_payload(&payload);
            let mut node = template.clone();
            node.node_id = section.into();
            node.text = text.clone();
            node.total_chars = text.chars().count();
            node.presentation =
                crate::presentation::project("interruption_details", &payload, &text, 0);
            assert_eq!(entry(&node, 80).0, format!("前回の保存記録 · {label}"));
            ui.view_mut().nodes_mut_for_test().push(node);
        }
        let mut details = Details::new(ui.view(), false);
        let mut request = 0;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area(), ui.view());
            })
            .unwrap();
        let shown = screen(&terminal);
        for label in ["作業", "応答処理", "コマンド", "子の作業"] {
            assert!(
                shown.contains(&format!("前回の保存記録 · {label}")),
                "{shown}"
            );
        }
        let expected = details.node_id.clone();
        details.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ui.view(),
            &mut request,
        );
        let fields = details.fields(ui.view());
        for value in ["complete-saved-id", "running", "stopped", "true"] {
            assert!(fields.iter().any(|field| field.value == value), "{value}");
        }
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE), ui.view(), &mut request), Outcome::Action(action) if matches!(&*action,UiAction::LoadOriginal {node_id,..} if node_id==&expected))
        );
    }
    #[test]
    fn detail_target_is_latest_when_following_and_anchor_when_reading() {
        let mut ui = state();
        let mut latest = ui.view().nodes()[0].clone();
        latest.node_id = "latest".into();
        ui.view_mut().nodes_mut_for_test().push(latest);
        ui.view_mut().anchor.node_id = Some("source-node".into());
        for follow in [true, false] {
            ui.view_mut().follow = follow;
            let expected = if follow { "latest" } else { "source-node" };
            for links in [false, true] {
                let mut details = Details::new(ui.view(), links);
                let mut request = 0;
                assert_eq!(details.node_id, expected);
                let action = details.key(
                    KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
                    ui.view(),
                    &mut request,
                );
                assert!(
                    matches!(action, Outcome::Action(action) if matches!(&*action, UiAction::LoadOriginal {node_id,..} if node_id == expected))
                );
                details.key(
                    KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
                    ui.view(),
                    &mut request,
                );
                assert_eq!(details.node_id, expected);
                assert_eq!(details.selected, usize::from(follow));
                let action = details.key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
                    ui.view(),
                    &mut request,
                );
                assert!(
                    matches!(action, Outcome::Action(action) if matches!(&*action, UiAction::CopyText {text,..} if text == expected))
                );
            }
        }
    }
    #[test]
    fn long_field_preview_leaves_neighbors_visible_and_copy_keeps_the_complete_value() {
        let mut ui = state();
        let value = format!("本文先頭\n{}本文末尾", "中間の行\n".repeat(80));
        ui.view_mut().nodes_mut_for_test()[0].presentation.fields =
            crate::presentation::fields_of_value(&serde_json::json!({
                "status":"completed", "text":value
            }));
        for (width, height) in [(100, 30), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut details = Details::new(ui.view(), false);
            details.mode = Mode::Fields;
            terminal
                .draw(|frame| {
                    details.draw(frame, frame.area(), ui.view());
                })
                .unwrap();
            let text = screen(&terminal);
            for expected in ["本文先頭", "本文末尾", "…", "source-node"] {
                assert!(text.contains(expected), "{width}x{height}: {text}");
            }
            details.selected = details
                .fields(ui.view())
                .iter()
                .position(|f| f.key == "/text")
                .unwrap();
            assert!(
                matches!(details.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), ui.view(), &mut 0), Outcome::Action(action) if matches!(action.as_ref(), UiAction::CopyText{text,..} if text == &value))
            );
        }
    }
    #[test]
    fn metadata_bounds_are_visible_in_fields_without_claiming_body_truncation() {
        let mut ui = state();
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        for legacy in [false, true] {
            let p = &mut ui.view_mut().nodes_mut_for_test()[0].presentation;
            p.metadata_limited = !legacy;
            p.details_omitted = true;
            p.body_omitted_chars = 0;
            p.body_extent_unknown = false;
            let count = p.fields.len();
            let mut details = Details::new(ui.view(), false);
            details.mode = Mode::Fields;
            terminal
                .draw(|frame| {
                    details.draw(frame, frame.area(), ui.view());
                })
                .unwrap();
            let text = screen(&terminal);
            let expected = if legacy {
                "表示情報の保持範囲は未確認".to_owned()
            } else {
                format!("詳細{count}項目を保持・上限あり")
            };
            assert!(text.contains(&expected), "{text}");
            assert!(text.contains("A 対象を照会"), "{text}");
            assert!(!text.contains("本文・出力"), "{text}");
            details.mode = Mode::Nodes;
            terminal
                .draw(|frame| {
                    details.draw(frame, frame.area(), ui.view());
                })
                .unwrap();
            assert!(!screen(&terminal).contains(&expected));
        }
    }
    #[test]
    fn literal_marker_in_history_detail_is_a_complete_value_not_an_original_request() {
        let literal = "入力の [詳細は原文参照] は値そのもの";
        let mut ui = state();
        ui.view_mut().nodes_mut_for_test()[0].presentation.fields =
            crate::presentation::fields_of_value(&serde_json::json!({"note":literal}));
        let mut details = Details::new(ui.view(), false);
        details.mode = Mode::Fields;
        details.selected = details
            .fields(ui.view())
            .iter()
            .position(|field| field.key == "/note")
            .unwrap();
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'),KeyModifiers::NONE),ui.view(),&mut 0),Outcome::Action(action) if matches!(action.as_ref(),UiAction::CopyText{text,..} if text==literal))
        );
    }
    #[test]
    fn entries_identify_content_and_selection_controls_original_copy_and_return() {
        let mut ui = state();
        *ui.view_mut().nodes_mut_for_test() = [
            ("first", "user_message", "設定ファイルを確認して"),
            ("second", "assistant_response", "接続先を確認しました"),
        ]
        .into_iter()
        .map(|(id, kind, text)| super::super::UiNode {
            node_id: id.into(),
            role: if kind == "user_message" {
                "user"
            } else {
                "assistant"
            }
            .into(),
            text: text.into(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: crate::presentation::project(
                kind,
                &serde_json::json!({"text":text}),
                text,
                0,
            ),
        })
        .collect();
        let mut details = Details::new(ui.view(), false);
        let mut next = 0;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area(), ui.view());
            })
            .unwrap();
        let text = screen(&terminal);
        for expected in [
            "会話の詳細",
            "設定ファイルを確認して",
            "接続先を確認しました",
            "あなた",
            "モデルの回答",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        details.key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert_eq!(details.node_id, "first");
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), ui.view(), &mut next), Outcome::Action(action) if matches!(&*action, UiAction::CopyText{text,..} if text=="first"))
        );
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE), ui.view(), &mut next), Outcome::Action(action) if matches!(&*action, UiAction::LoadOriginal{node_id,..} if node_id=="first"))
        );
        details.key(
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert_eq!(details.selected, 0);
        details.key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        details.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area(), ui.view());
            })
            .unwrap();
        assert!(screen(&terminal).contains("接続先を確認しました"));
        details.key(
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert_eq!(details.selected, 1);
        assert_eq!(details.node_id, "second");
    }
    #[test]
    fn grouped_fields_copy_the_original_time_and_complete_reference_not_display_text() {
        let mut ui = state();
        let value = serde_json::json!({"state":"running","execution_id":"complete-execution-reference","started_at":"2026-10-25T00:00:00Z","reason":null});
        let mut fields = crate::presentation::fields_of_value(&value);
        crate::presentation::fields::decorate(&mut fields, &value);
        ui.view_mut().nodes_mut_for_test()[0].presentation.fields = fields;
        let mut details = Details::new(ui.view(), false);
        details.mode = Mode::Fields;
        details.node_id = ui.view().nodes()[0].node_id.clone();
        details.selected = details
            .fields(ui.view())
            .iter()
            .position(|f| f.key == "/started_at")
            .unwrap();
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'),crossterm::event::KeyModifiers::NONE),ui.view(),&mut 0),Outcome::Action(action) if matches!(action.as_ref(),UiAction::CopyText{text,..} if text=="2026-10-25T00:00:00Z"))
        );
        details.selected = details
            .fields(ui.view())
            .iter()
            .position(|f| f.key == "/execution_id")
            .unwrap();
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'),crossterm::event::KeyModifiers::NONE),ui.view(),&mut 0),Outcome::Action(action) if matches!(action.as_ref(),UiAction::CopyText{text,..} if text=="complete-execution-reference"))
        );
        assert!(matches!(
            details.key(
                KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE),
                ui.view(),
                &mut 0
            ),
            Outcome::Operation(super::super::operations::OpKind::ShellGet)
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area(), ui.view());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut shown = String::new();
        for y in 0..24 {
            let mut x = 0;
            while x < 90 {
                let symbol = buffer[(x, y)].symbol();
                shown.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            shown.push('\n');
        }
        for group in ["要点", "実行", "参照"] {
            assert!(shown.contains(group));
        }
        assert!(!shown.contains("/started_at"));
    }
    #[test]
    fn entry_uses_projected_shell_command_and_bounds_unicode_preview() {
        let mut node = state().view().nodes()[0].clone();
        let command = "printf '日本語の確認'";
        let payload = serde_json::json!({"type":"function_call", "name":"shell", "arguments":serde_json::json!({"command":command}).to_string()});
        node.text = payload.to_string();
        node.presentation = crate::presentation::project(
            "assistant_response",
            &payload,
            &serde_json::json!({"command":command}).to_string(),
            0,
        );
        let (label, preview) = entry(&node, 80);
        assert!(label.contains("shell呼出し"));
        assert_eq!(preview, command);
        node.presentation = Default::default();
        node.text = "長い日本語".repeat(300);
        let (_, preview) = entry(&node, 24);
        assert!(unicode_width::UnicodeWidthStr::width(preview.as_str()) <= 24);
        assert!(preview.ends_with('…'));
        node.text.clear();
        assert!(entry(&node, 80).1.contains("未読込"));
        node.presentation.kind = crate::presentation::PresentationKind::Assistant;
        node.presentation.blocks = vec![
            crate::presentation::PresentationBlock {
                kind: crate::presentation::BlockKind::Reasoning,
                text: "内部の検討".into(),
                ..Default::default()
            },
            crate::presentation::PresentationBlock {
                kind: crate::presentation::BlockKind::Prose,
                text: "確認できた回答".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            entry(&node, 80),
            ("モデルの回答".into(), "確認できた回答".into())
        );
    }
    #[test]
    fn selected_last_url_has_fixed_controls_exact_copy_and_matching_hit_cells() {
        let ui = state();
        let anchor = ui.view().anchor.clone();
        let draft = ui.view().draft.clone();
        for (width, height) in [(80, 24), (32, 12), (8, 5)] {
            let mut details = Details::new(ui.view(), true);
            details.selected = 29;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut regions = Vec::new();
            terminal
                .draw(|frame| regions = details.draw(frame, frame.area(), ui.view()))
                .unwrap();
            let text = screen(&terminal);
            assert!(text.contains("Esc"), "{width}x{height}: {text}");
            if width >= 32 {
                assert!(
                    text.replace(' ', "").contains("リンク29"),
                    "last selected URL must be visible: {text}"
                );
                assert!(!regions.is_empty());
                let url = &ui.view().nodes()[0].presentation.links[29].url;
                assert!(regions.iter().any(|region| region.url.as_ref() == url));
                for region in &regions {
                    assert!(
                        ui.view().nodes()[0]
                            .presentation
                            .links
                            .iter()
                            .any(|link| link.url == region.url.as_ref()),
                        "visible hit target retains a real URL"
                    );
                    assert!(region.y < height && region.x + region.width <= width);
                    for x in region.x..region.x + region.width {
                        assert_eq!(
                            terminal.backend().buffer()[(x, region.y)].underline_color,
                            Color::Reset
                        );
                    }
                }
                let mut next = 0;
                let copied = details.key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
                    ui.view(),
                    &mut next,
                );
                assert!(
                    matches!(copied,Outcome::Action(action) if matches!(&*action,UiAction::CopyText{text,..} if text==url))
                );
            }
        }
        assert_eq!(ui.view().anchor, anchor);
        assert_eq!(ui.view().draft, draft);
    }
    #[test]
    fn list_pages_move_selection_while_original_pages_scroll_and_reject_stale_data() {
        let ui = state();
        let mut details = Details::new(ui.view(), true);
        let mut next = 0;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area(), ui.view());
            })
            .unwrap();
        details.key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert!(details.selected > 0);
        assert_eq!(details.scroll.get(), 0);
        let action = details.key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert!(
            matches!(action,Outcome::Action(action) if matches!(&*action,UiAction::LoadOriginal{node_id,..}if node_id=="source-node"))
        );
        let body = HistoryBody {
            node_id: "source-node".into(),
            kind: "assistant_response".into(),
            text: (0..100).map(|index| format!("row {index:03}\n")).collect(),
            context: serde_json::from_value(serde_json::json!({"state":"present","node_id":"source-node","created_at":"2026-10-25T00:00:00Z"})).unwrap(),
            provenance: crate::protocol::NodeProvenance {
                kind: "assistant_response".into(),
                created_at: "2026-10-25T00:00:00Z".into(),
                source_revision: 0,
                turn_id: None,
                run_id: None,
                origin_thread_id: None,
            },
            source_node_ids: vec![],
            previous_node_ids: vec![],
            next_node_ids: vec![],
            next_cursor: None,
            tombstone_reason: None,
        };
        details.accept(next + 1, body.clone(), original_page_lines(&body));
        assert!(details.original.is_none());
        let display = original_page_lines(&body);
        details.accept(next, body, display);
        for _ in 0..100 {
            details.key(
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                ui.view(),
                &mut next,
            );
        }
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area(), ui.view());
            })
            .unwrap();
        let at = details.scroll.get();
        assert!(at > 0 && at < 110);
        assert!(screen(&terminal).contains("row 099"));
        details.key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert_eq!(details.scroll.get(), at - 1);
    }
    #[test]
    fn known_answer_details_hide_redundant_structure_but_keep_full_copy_unknowns_and_source() {
        let payload = serde_json::json!({"group_id":null,"reasoning_epoch":0,"future":null,"output":[{
            "type":"message","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":"完全な本文 e\u{301} 🦀"}]}]});
        let raw = crate::history::text_of_payload(&payload);
        let mut ui = state();
        let node = &mut ui.view_mut().nodes_mut_for_test()[0];
        node.text = raw.clone();
        node.total_chars = raw.chars().count();
        node.presentation = crate::presentation::project("assistant_response", &payload, &raw, 0);
        let raw_fields = serde_json::to_value(&node.presentation.fields).unwrap();
        let mut details = Details::new(ui.view(), false);
        let mut next = 0;
        details.key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        let fields = details.fields(ui.view());
        assert!(!fields.iter().any(|f| matches!(
            f.key.as_str(),
            "/group_id"
                | "/reasoning_epoch"
                | "/output"
                | "/output/0/content"
                | "/output/0/type"
                | "/output/0/role"
                | "/output/0/content/0/type"
        )));
        assert!(
            fields
                .iter()
                .any(|f| f.key == "/future" && f.value == "null")
        );
        let body = fields
            .iter()
            .position(|f| f.key == "/output/0/content/0/text")
            .unwrap();
        assert_eq!(fields[body].label, "本文");
        details.selected = body;
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'),KeyModifiers::NONE),ui.view(),&mut next),
            Outcome::Action(action) if matches!(action.as_ref(),UiAction::CopyText{text,..} if text=="完全な本文 e\u{301} 🦀"))
        );
        for width in [60, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| {
                    details.draw(frame, frame.area(), ui.view());
                })
                .unwrap();
            let visible = screen(&terminal);
            assert!(visible.contains("本文:"));
            assert!(!visible.contains("項目 1 › 項目 1"));
            assert!(!visible.contains("呼出し群ID: null"));
            assert!(visible.contains("O 原文"));
        }
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('o'),KeyModifiers::NONE),ui.view(),&mut next),
            Outcome::Action(action) if matches!(action.as_ref(),UiAction::LoadOriginal{node_id,..} if node_id=="source-node"))
        );
        // The internal view exposes every retained original field, including defaults.
        details.mode = Mode::Fields;
        details.key(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        let fields = details.fields(ui.view());
        assert!(
            fields
                .iter()
                .any(|f| f.key == "/group_id" && f.value == "null")
        );
        details.selected = fields
            .iter()
            .position(|f| f.key == "/reasoning_epoch")
            .unwrap();
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'),KeyModifiers::NONE),ui.view(),&mut next),
            Outcome::Action(action) if matches!(action.as_ref(),UiAction::CopyText{text,..} if text=="0"))
        );
        details.key(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
            ui.view(),
            &mut next,
        );
        assert!(
            !details
                .fields(ui.view())
                .iter()
                .any(|f| f.key == "/group_id")
        );
        assert_eq!(
            serde_json::to_value(&ui.view().nodes()[0].presentation.fields).unwrap(),
            raw_fields
        );
        // Multiple known messages retain their source item/content numbering.
        let payload = serde_json::json!({"reasoning_epoch":1,"items":[
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"first"},{"type":"output_text","text":"second"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"third"}]}]});
        ui.view_mut().nodes_mut_for_test()[0].presentation = crate::presentation::project(
            "assistant_response",
            &payload,
            &crate::history::text_of_payload(&payload),
            0,
        );
        let fields = conversation_fields(&ui.view().nodes()[0]);
        for (label, value) in [
            ("回答 1 › 本文 1", "first"),
            ("回答 1 › 本文 2", "second"),
            ("回答 2 › 本文", "third"),
        ] {
            assert!(fields.iter().any(|f| f.label == label && f.value == value));
        }
        assert!(
            fields
                .iter()
                .any(|f| f.key == "/reasoning_epoch" && f.value == "1")
        );
        let message = serde_json::json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"nested"}]});
        let payload = serde_json::json!({"response":{"output":[message.clone()]}});
        ui.view_mut().nodes_mut_for_test()[0].presentation =
            crate::presentation::project("assistant_response", &payload, "nested", 0);
        assert!(
            conversation_fields(&ui.view().nodes()[0])
                .iter()
                .any(|f| f.key == "/response/output/0/content/0/text" && f.label == "本文")
        );
        let payload = serde_json::json!({"output":[message.clone()],"items":[message]});
        ui.view_mut().nodes_mut_for_test()[0].presentation =
            crate::presentation::project("assistant_response", &payload, "nested", 0);
        assert_eq!(
            serde_json::to_value(conversation_fields(&ui.view().nodes()[0])).unwrap(),
            serde_json::to_value(&ui.view().nodes()[0].presentation.fields).unwrap(),
            "ambiguous collections retain their original field structure"
        );
        let payload = serde_json::json!({"output":[{"type":"message","role":"assistant"}],"item":{"type":"message","role":"assistant"}});
        ui.view_mut().nodes_mut_for_test()[0].presentation =
            crate::presentation::project("assistant_response", &payload, "", 0);
        assert_eq!(
            serde_json::to_value(conversation_fields(&ui.view().nodes()[0])).unwrap(),
            serde_json::to_value(&ui.view().nodes()[0].presentation.fields).unwrap()
        );
        let payload = serde_json::json!({"items":{"18446744073709551615":{"type":"message","role":"assistant","content":{"18446744073709551615":{"type":"output_text","text":"opaque"}}}}});
        ui.view_mut().nodes_mut_for_test()[0].presentation =
            crate::presentation::project("assistant_response", &payload, "opaque", 0);
        assert_eq!(
            serde_json::to_value(conversation_fields(&ui.view().nodes()[0])).unwrap(),
            serde_json::to_value(&ui.view().nodes()[0].presentation.fields).unwrap()
        );
        let payload = serde_json::json!({"items":[{"type":"future_item","role":"assistant","content":[{"type":"future_text","text":"opaque"}]}]});
        ui.view_mut().nodes_mut_for_test()[0].presentation =
            crate::presentation::project("assistant_response", &payload, "opaque", 0);
        assert_eq!(
            serde_json::to_value(conversation_fields(&ui.view().nodes()[0])).unwrap(),
            serde_json::to_value(&ui.view().nodes()[0].presentation.fields).unwrap()
        );
    }
    #[test]
    fn original_page_is_prepared_once_and_reused_across_frames_and_widths() {
        let ui = state();
        let mut details = Details::new(ui.view(), false);
        details.node_id = "source-node".into();
        details.mode = Mode::Original;
        details.request_id = 9;
        let text = "\x1b[31m\t日本語の原文\n".repeat(2000);
        let body = HistoryBody {
            node_id: "source-node".into(),
            kind: "assistant_response".into(),
            text: text.clone(),
            context: serde_json::from_value(serde_json::json!({"state":"present","node_id":"source-node","created_at":"2026-10-25T00:00:00Z"})).unwrap(),
            provenance: crate::protocol::NodeProvenance {
                kind: "assistant_response".into(),
                created_at: "2026-10-25T00:00:00Z".into(),
                source_revision: 0,
                turn_id: None,
                run_id: None,
                origin_thread_id: None,
            },
            source_node_ids: vec!["source-id".into()],
            previous_node_ids: vec![],
            next_node_ids: vec![],
            next_cursor: Some("next".into()),
            tombstone_reason: None,
        };
        let display = original_page_lines(&body);
        assert!(
            display
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| !span.content.contains('\x1b'))
        );
        details.accept(9, body.clone(), display);
        let pointer = details.original_lines.as_ptr();
        let prepared = ORIGINAL_PREPARATIONS.with(Cell::get);
        for width in [35, 80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            for _ in 0..5 {
                terminal
                    .draw(|frame| {
                        details.draw(frame, frame.area(), ui.view());
                    })
                    .unwrap();
            }
        }
        assert_eq!(ORIGINAL_PREPARATIONS.with(Cell::get), prepared);
        assert_eq!(details.original_lines.as_ptr(), pointer);
        details.accept(8, body, vec![Line::from("stale display")]);
        assert_eq!(details.original_lines.as_ptr(), pointer);
        assert!(
            !details
                .original_lines
                .iter()
                .any(|line| line.to_string().contains("stale display"))
        );
        let mut next = 9;
        assert!(
            matches!(details.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), ui.view(), &mut next), Outcome::Action(action) if matches!(*action, UiAction::CopyText { text: ref copied, .. } if copied == &text))
        );
    }
}
