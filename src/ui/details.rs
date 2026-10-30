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
use std::cell::Cell;
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
    item_scoped: bool,
    extra_links: Vec<crate::presentation::PresentationLink>,
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
    if let Some(sender) = &body.provenance.sender {
        lines.extend(sender_lines(sender));
    }
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
fn sender_lines(sender: &crate::protocol::MessageSender) -> Vec<Line<'static>> {
    sender_fields(sender)
        .into_iter()
        .map(|field| Line::from(format!("{}：{}", field.label, safe(&field.value))))
        .collect()
}
fn sender_fields(sender: &crate::protocol::MessageSender) -> Vec<PresentationField> {
    let kind = match sender.kind {
        crate::protocol::SenderKind::Model => "model (verified Origin)",
        crate::protocol::SenderKind::ExternalCli => "external_cli",
        crate::protocol::SenderKind::Human => "human (TUI)",
        crate::protocol::SenderKind::Unknown => "unknown",
    };
    let mut fields = vec![("sender.kind", "送り手", kind.to_owned())];
    for (key, value) in [
        ("instance_id", &sender.instance_id),
        ("thread_id", &sender.thread_id),
        ("run_id", &sender.run_id),
        ("turn_id", &sender.turn_id),
        ("execution_id", &sender.execution_id),
    ] {
        if let Some(value) = value {
            fields.push((key, key, value.clone()));
        }
    }
    if let Some(label) = &sender.label {
        fields.push(("sender.label", "名乗り", label.clone()));
    }
    fields
        .into_iter()
        .map(|(key, label, value)| PresentationField {
            key: if key.starts_with("sender.") {
                key.into()
            } else {
                format!("sender.{key}")
            },
            label: if key.ends_with("_id") {
                format!("送り手 {label}")
            } else {
                label.into()
            },
            value,
            display: None,
            value_omitted: false,
        })
        .collect()
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
            item_scoped: false,
            extra_links: vec![],
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
            original_lines: Vec::new(),
            revision,
        }
    }
    pub(super) fn for_node(view: &ThreadView, node_id: String, links: bool) -> Self {
        let mut details = Self::new(view, links);
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
    fn links(&self, view: &ThreadView) -> Vec<crate::presentation::PresentationLink> {
        let mut links = Vec::new();
        for id in &self.source_node_ids {
            if let Some(node) = view.nodes().iter().find(|node| &node.node_id == id) {
                for link in &node.presentation.links {
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
        }
        for link in &self.extra_links {
            if links.len() >= 128 {
                break;
            }
            if !links.iter().any(|saved| saved.url == link.url) {
                links.push(link.clone());
            }
        }
        links
    }
    pub(super) fn request_links(
        &mut self,
        thread_id: String,
        generation: u64,
        next: &mut u64,
    ) -> UiAction {
        *next = next.wrapping_add(1);
        self.links_request_id = *next;
        self.links_pending = true;
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
        result: Result<Vec<crate::presentation::PresentationLink>, String>,
    ) {
        if self.links_request_id != request_id || !self.links_pending {
            return;
        }
        self.links_pending = false;
        match result {
            Ok(links) => self.extra_links = links.into_iter().take(128).collect(),
            Err(error) => self.links_error = Some(error),
        }
    }
    pub fn accept(&mut self, request_id: u64, body: HistoryBody, display: Vec<Line<'static>>) {
        if request_id == self.request_id && body.node_id == self.node_id {
            self.original = Some(body);
            self.original_lines = display;
            self.scroll.set(0);
        }
    }
    fn fields(&self, view: &ThreadView) -> Vec<PresentationField> {
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
        if let Some(sender) = self
            .original
            .as_ref()
            .and_then(|body| body.provenance.sender.as_ref())
        {
            fields.extend(sender_fields(sender));
        }
        fields.sort_by_key(|field| crate::presentation::fields::group(&field.key).0);
        fields
    }
    pub(super) fn open_source(&mut self, node_id: String, next: &mut u64) -> UiAction {
        self.node_id = node_id;
        self.original(None, next)
    }
    pub(super) fn load_sender(&mut self, next: &mut u64) -> UiAction {
        *next = next.wrapping_add(1);
        self.request_id = *next;
        UiAction::LoadOriginal {
            node_id: self.node_id.clone(),
            cursor: None,
            request_id: self.request_id,
        }
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
                        return Outcome::Action(Box::new(self.load_sender(next)));
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
                            self.node_id = field.value.clone();
                            return Outcome::Action(Box::new(self.original(None, next)));
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
            self.node_id = node.node_id.clone();
            self.source_node_ids = vec![self.node_id.clone()];
            self.links_request_id = 0;
            self.links_pending = false;
            self.links_error = None;
            self.extra_links.clear();
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
            format!("node {}", safe(&self.node_id))
        } else {
            format!(
                "{} / {}件 · node {}",
                self.selected.saturating_add(1).min(count),
                count,
                safe(&self.node_id)
            )
        };
        let footer = if matches!(self.mode, Mode::Original) {
            "Esc 戻る · ↑↓/PgUp/PgDn スクロール\nN 次page · P 先頭 · C 原文copy · F6 項目 · F7 リンク"
        } else if matches!(self.mode, Mode::Fields) {
            "Esc 戻る · ↑↓/PgUp/PgDn 選択 · Enter 詳細\nO 原文 · C 値copy · V 要点/内部項目 · A 操作"
        } else {
            "Esc 戻る · ↑↓/PgUp/PgDn 選択 · Enter 詳細\nO 原文 · C 値copy · A 操作 · F6 項目 · F7 リンク"
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
        if matches!(self.mode, Mode::Links)
            && body.height > 1
            && (self.links_pending || self.links_error.is_some())
        {
            let notice = self
                .links_error
                .as_deref()
                .unwrap_or("出典のリンクを取得中");
            frame.render_widget(
                ratatui::widgets::Paragraph::new(safe(notice))
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
                            &format!(
                                "{}{}{}",
                                safe(&field.label),
                                if field.key == "sender.label" {
                                    "："
                                } else {
                                    ": "
                                },
                                safe(field.displayed())
                            ),
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
        details.accept_links(token - 1, Ok(vec![extra.clone()]));
        assert!(details.links_pending && details.extra_links.is_empty());
        details.accept_links(token, Ok(vec![extra.clone()]));
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
            Ok(vec![PresentationLink {
                label: "later".into(),
                url: "https://example.invalid/later".into(),
                ..Default::default()
            }]),
        );
        assert_eq!(details.extra_links.len(), 1);
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
            Ok(vec![PresentationLink {
                url: "https://example.invalid/old".into(),
                ..Default::default()
            }]),
        );
        details.request_links("anonymous".into(), ui.view().generation, &mut next);
        let pending = details.links_request_id;
        details.selected = 1;
        details.sync_node(ui.view());
        assert_eq!(details.node_id, "second");
        assert!(details.extra_links.is_empty());
        assert!(!details.links_pending);
        assert_eq!(details.links_request_id, 0);
        details.accept_links(
            pending,
            Ok(vec![PresentationLink {
                url: "https://example.invalid/stale".into(),
                ..Default::default()
            }]),
        );
        assert!(details.links(ui.view()).is_empty());
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
            provenance: crate::protocol::NodeProvenance { sender: None,
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
            provenance: crate::protocol::NodeProvenance { sender: None,
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
    #[test]
    fn sender_is_only_added_to_details_and_self_asserted_name_is_distinct() {
        use crate::protocol::{MessageSender, SenderKind};
        let ui = state();
        let mut details = Details::for_node(ui.view(), "source-node".into(), false);
        let before = serde_json::to_value(&ui.view().nodes()[0].presentation.fields).unwrap();
        let sender = MessageSender {
            kind: SenderKind::ExternalCli,
            label: Some("leader".into()),
            ..Default::default()
        };
        let mut next = 0;
        let action = details.load_sender(&mut next);
        assert!(matches!(
            action,
            UiAction::LoadOriginal { cursor: None, .. }
        ));
        assert!(matches!(details.mode, Mode::Fields));
        let body: HistoryBody = serde_json::from_value(serde_json::json!({
            "context":{"state":"present","node_id":"source-node","created_at":"now"},
            "provenance":{"kind":"user","created_at":"now","source_revision":0,"sender":sender},
            "node_id":"source-node","kind":"user","text":"source input",
            "source_node_ids":[],"previous_node_ids":[],"next_node_ids":[]
        }))
        .unwrap();
        let lines = original_page_lines(&body);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("名乗り：leader"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("external_cli"))
        );
        details.accept(next + 1, body.clone(), lines.clone());
        assert!(
            details.original.is_none(),
            "stale replies must not provide sender facts"
        );
        details.accept(next, body, lines);
        let fields = details.fields(ui.view());
        assert!(
            fields
                .iter()
                .any(|field| field.key == "sender.kind" && field.value == "external_cli")
        );
        assert!(fields.iter().any(|field| field.key == "sender.label"
            && field.label == "名乗り"
            && field.value == "leader"));
        assert_eq!(
            serde_json::to_value(&ui.view().nodes()[0].presentation.fields).unwrap(),
            before
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area(), ui.view());
            })
            .unwrap();
        let shown = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        // Wide glyph continuation cells are blanks in the TestBackend buffer.
        let compact = shown
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        assert!(compact.contains("名乗り：leader"), "{shown}");
    }
}
