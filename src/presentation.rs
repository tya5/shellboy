//! Bounded, immutable display projection. Canonical history and raw bytes stay untouched.
/// Shared finite blocking admission for display projections and read-only CLI facts.
pub(crate) async fn work<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, crate::protocol::ApiError> {
    use crate::protocol::{ApiError, ErrorCode};
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    let permit = SLOTS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Presentation workers closed"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Presentation worker failed"))
}

mod events;
pub(crate) mod fields;
mod markdown_links;
mod projection;
pub use projection::project;
mod summary;
use crate::terminal_links::valid_link_target as http;
use events::{attention, event_description};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::mem::size_of;

/// Human-readable recorded Revisit facts shared by existing displays.
pub fn revisit_label(summary: &crate::protocol::RevisitSummary) -> Option<String> {
    if summary.consumed_count == 0 && !summary.pending {
        return None;
    }
    let mut label = format!("Revisit {}", summary.consumed_count);
    if summary.pending {
        label.push_str(" · next reserved");
    }
    Some(label)
}

pub const MAX_METADATA_BYTES: usize = 64 * 1024;
pub(crate) const MAX_FIELDS: usize = 128;
pub(crate) const CONTEXT_TRIM_TITLE: &str = "入力範囲を縮小（原文は保持）";
const MAX_LINKS: usize = 128;
const MAX_BLOCKS: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresentationKind {
    User,
    Assistant,
    ToolCall,
    ToolResult,
    Event,
    Summary,
    Spill,
    Interrupted,
    #[default]
    Unknown,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    #[default]
    Plain,
    Prose,
    Reasoning,
    Code,
    ToolCall,
    ToolResult,
    Event,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkdownKind {
    #[default]
    Text,
    Heading(u8),
    Paragraph,
    List(Option<u64>),
    ListItem,
    Quote,
    CodeBlock(String),
    Code,
    Strong,
    Emphasis,
    Link(String),
    SoftBreak,
    HardBreak,
    Rule,
    TableRow,
    TableCell,
    End,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownToken {
    pub kind: MarkdownKind,
    pub text: String,
    pub source_start: usize,
    pub source_end: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
    Combined,
}
/// Display classification of saved reason values. Legacy text is decoded only here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolReasonKind {
    #[default]
    Unknown,
    RunStopped,
    PriorCallInterrupted,
    PriorCallFailed,
    PriorOutputIncomplete,
    PriorWaitNewInput,
    PriorWaitHumanBackground,
    PriorWaitReleased,
    HumanTurnInterrupted,
    PriorTurnInterrupted,
    OwnerShutdown,
    GroupValidation,
    AdditionalInput,
    HumanBackgrounded,
    ForegroundTimeout,
    OutputLimit,
}
impl ToolReasonKind {
    pub fn from_saved(value: &str) -> Self {
        match value {
            "Run stopped before this call" | "run_stopped" | "run_stopped_before_call" => {
                Self::RunStopped
            }
            "Previous tool failed or foreground wait was interrupted" => Self::PriorCallInterrupted,
            "Tool group validation failed before any side effect" => Self::GroupValidation,
            "previous_call_failed" => Self::PriorCallFailed,
            "previous_output_incomplete" => Self::PriorOutputIncomplete,
            "previous_wait_released_by_new_input" => Self::PriorWaitNewInput,
            "previous_wait_released_by_human_background" => Self::PriorWaitHumanBackground,
            "previous_wait_released" => Self::PriorWaitReleased,
            "human_turn_interrupted" => Self::HumanTurnInterrupted,
            "previous_turn_interrupted" => Self::PriorTurnInterrupted,
            "owner_shutdown_before_call" => Self::OwnerShutdown,
            "tool_group_validation_failed" => Self::GroupValidation,
            "additional_input" => Self::AdditionalInput,
            "human_backgrounded" => Self::HumanBackgrounded,
            "foreground_wait_timeout" => Self::ForegroundTimeout,
            "output_limit" => Self::OutputLimit,
            _ => Self::Unknown,
        }
    }
}
/// UI-only identity and facts for one call; canonical payloads remain unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPresentation {
    #[serde(skip)]
    pub group_id: Option<String>,
    #[serde(skip)]
    pub call_node_id: Option<String>,
    #[serde(skip)]
    pub result_node_id: Option<String>,
    pub call_id: String,
    pub ordinal: Option<u32>,
    pub command: String,
    pub status: String,
    #[serde(default)]
    pub current_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_exit_code: Option<i64>,
    pub reason: String,
    #[serde(default)]
    pub reason_code: String,
    #[serde(default)]
    pub reason_kind: ToolReasonKind,
    pub execution_id: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub exit_code: Option<i64>,
}
impl ToolPresentation {
    pub fn heading(&self) -> String {
        let current = self
            .current_state
            .as_deref()
            .filter(|state| *state != "observation_unavailable");
        let (state, code) = current.map_or((self.status.as_str(), self.exit_code), |state| {
            (state, self.current_exit_code)
        });
        let (mut mark, _) = tool_state_heading(state, code);
        if tool_failed(&self.status, self.exit_code) {
            mark = "✗";
        }
        let state_text = tool_state_text(state, code);
        let identity_command = self.command_identity();
        let observed = if current.is_some_and(|state| {
            matches!(
                state,
                "starting" | "running_current" | "background_current" | "stopping"
            )
        }) {
            "現在 "
        } else {
            ""
        };
        let mut heading = format!("{mark} {identity_command} · {observed}{state_text}");
        if let Some(current) = current {
            let same_state = current == self.status
                || (matches!(current, "running_current" | "background_current")
                    && self.status == "running")
                || (current == "exited"
                    && tool_failed(&self.status, self.exit_code)
                    && self.exit_code.is_some_and(|code| code != 0));
            if !matches!(self.status.as_str(), "requested" | "completed")
                && !(same_state && self.current_exit_code == self.exit_code)
            {
                let returned = tool_state_text(&self.status, self.exit_code);
                heading.push_str(&format!(
                    " · 返却時 {}",
                    returned.strip_suffix("（返却時）").unwrap_or(&returned)
                ));
            }
        } else if self.current_state.as_deref() == Some("observation_unavailable") {
            heading.push_str(" · 現在状態は未確認");
        }
        if let Some(ms) = self.elapsed_ms {
            heading.push_str(&format!(" · {}.{:01}秒", ms / 1000, (ms % 1000) / 100));
        }
        if let Some(id) = &self.execution_id {
            heading.push_str(&format!(" · 実行 {} · F6", compact_tool_label(id, 8)));
        }
        heading
    }
    fn command_identity(&self) -> String {
        let identity = self.ordinal.map_or_else(
            || format!("参照 {}", compact_tool_label(&self.call_id, 16)),
            |n| format!("#{}", n + 1),
        );
        let command = if self.command.is_empty() {
            "コマンドは原文参照".into()
        } else {
            compact_tool_label(
                &self
                    .command
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                120,
            )
        };
        format!("{identity} {command}")
    }
    /// A call shows the request; exit facts belong to its result card.
    pub fn call_heading(&self) -> String {
        let live = self.current_state.as_deref().filter(|state| {
            matches!(
                *state,
                "pending" | "starting" | "running_current" | "background_current" | "stopping"
            )
        });
        let state = live
            .map(|state| format!(" · {}", tool_state_text(state, None)))
            .unwrap_or_default();
        format!("↳ {}{state}", self.command_identity())
    }
    pub fn clear_observation(&mut self) {
        self.current_state = None;
        self.current_exit_code = None;
    }
    pub fn observation_unavailable(&mut self) {
        self.clear_observation();
        self.current_state = Some("observation_unavailable".into());
    }
    pub fn requires_attention(&self) -> bool {
        tool_failed(&self.status, self.exit_code)
            || matches!(
                self.status.as_str(),
                "cancelled" | "stopped" | "interrupted" | "not_started"
            )
            || self.current_state.as_deref().is_some_and(|state| {
                tool_failed(state, self.current_exit_code)
                    || matches!(state, "cancelled" | "stopped")
            })
    }
    fn retained_bytes(&self) -> usize {
        self.call_id.capacity()
            + self.group_id.as_ref().map_or(0, String::capacity)
            + self.call_node_id.as_ref().map_or(0, String::capacity)
            + self.result_node_id.as_ref().map_or(0, String::capacity)
            + self.command.capacity()
            + self.status.capacity()
            + self.current_state.as_ref().map_or(0, String::capacity)
            + self.reason.capacity()
            + self.reason_code.capacity()
            + self.execution_id.as_ref().map_or(0, String::capacity)
    }
}
fn compact_tool_label(text: &str, limit: usize) -> String {
    let mut value: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        value.push('…');
    }
    value
}

fn tool_state_heading(state: &str, exit_code: Option<i64>) -> (&'static str, &'static str) {
    match state {
        "requested" => ("↳", "呼出し"),
        "pending" => ("◌", "待機中"),
        "starting" | "running_current" => ("◐", "実行中"),
        "running" => ("↗", "背景継続（返却時）"),
        "background_current" => ("↗", "背景で継続中"),
        "stopping" => ("◐", "停止要求済み"),
        "completed" => ("·", "結果あり"),
        "exited" => match exit_code {
            Some(0) => ("✓", "終了"),
            Some(_) => ("✗", "終了"),
            None => ("·", "終了（コード不明）"),
        },
        "failed" | "error" => ("✗", "失敗"),
        "cancelled" => ("⊘", "未実行・取消"),
        "not_started" => ("⊘", "未開始"),
        "interrupted" => ("!", "中断・結果不確か"),
        "stopped" => ("⊘", "停止"),
        _ => ("·", "状態は詳細参照"),
    }
}
fn tool_failed(state: &str, code: Option<i64>) -> bool {
    matches!(state, "failed" | "error") || (state == "exited" && code.is_some_and(|code| code != 0))
}
fn tool_state_text(state: &str, code: Option<i64>) -> String {
    let (_, label) = tool_state_heading(state, code);
    code.map_or_else(|| label.into(), |code| format!("{label} {code}"))
}
fn tool_reason(value: &str) -> String {
    match ToolReasonKind::from_saved(value) {
        ToolReasonKind::RunStopped => "作業停止のため取消".into(),
        ToolReasonKind::PriorCallInterrupted => {
            "先行する呼出しの失敗、または前景待機の中断により取消".into()
        }
        ToolReasonKind::PriorCallFailed => "先行する呼出しの失敗により未実行".into(),
        ToolReasonKind::PriorOutputIncomplete => "先行する呼出しの出力が不完全なため未実行".into(),
        ToolReasonKind::PriorWaitNewInput => {
            "追加の指示で先行する呼出しの待機を解除したため未実行".into()
        }
        ToolReasonKind::PriorWaitHumanBackground => {
            "利用者が先行する呼出しを背景へ移したため未実行".into()
        }
        ToolReasonKind::PriorWaitReleased => "先行する呼出しの待機結果により未実行".into(),
        ToolReasonKind::HumanTurnInterrupted => "利用者がTurnを中断".into(),
        ToolReasonKind::PriorTurnInterrupted => "利用者がTurnを中断したため未実行".into(),
        ToolReasonKind::OwnerShutdown => "アプリ終了処理のため未実行".into(),
        ToolReasonKind::GroupValidation => "呼出し群の検証失敗により未実行".into(),
        ToolReasonKind::AdditionalInput => "追加の指示を受けて背景へ移行".into(),
        ToolReasonKind::HumanBackgrounded => "利用者の操作で背景へ移行".into(),
        ToolReasonKind::ForegroundTimeout => "前景の待機時間上限で背景へ移行".into(),
        ToolReasonKind::OutputLimit => "出力上限に到達".into(),
        ToolReasonKind::Unknown => sanitize_plain(&short(value, 256), 0).0,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationBlock {
    pub kind: BlockKind,
    #[serde(default)]
    pub stream: Option<OutputStream>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolPresentation>,
    pub text: String,
    pub source_start: usize,
    pub source_end: usize,
    pub source_exact: bool,
    pub markdown: Vec<MarkdownToken>,
    #[serde(skip)]
    pub rendered: Option<RenderedMarkdown>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedMarkdown {
    pub width: u16,
    pub lines: Vec<Line<'static>>,
    pub omitted: bool,
}
impl RenderedMarkdown {
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.lines.capacity() * size_of::<Line<'static>>()
            + self
                .lines
                .iter()
                .map(|line| {
                    line.spans.capacity() * size_of::<Span<'static>>()
                        + line
                            .spans
                            .iter()
                            .map(|span| match &span.content {
                                Cow::Owned(text) => text.capacity(),
                                Cow::Borrowed(text) => text.len(),
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}
#[derive(Clone)]
struct ShellboyMarkdownStyle {
    code_marker: String,
    links: std::sync::Arc<markdown_links::LinkMarkers>,
}
impl tui_markdown::StyleSheet for ShellboyMarkdownStyle {
    fn heading(&self, _level: u8) -> Style {
        Style::default()
            .fg(crate::ui::theme::EMPHASIS)
            .add_modifier(Modifier::BOLD)
    }
    fn blockquote(&self) -> Style {
        Style::default().fg(crate::ui::theme::TEXT)
    }
    fn table_header(&self) -> Style {
        Style::default()
            .fg(crate::ui::theme::EMPHASIS)
            .add_modifier(Modifier::BOLD)
    }
    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }
    fn code(&self) -> Style {
        let mut style = Style::default()
            .fg(crate::ui::theme::CODE_TEXT)
            .bg(crate::ui::theme::CODE_BACKGROUND);
        style.underline_color = self.links.next_code();
        style
    }
    fn code_block_fence(&self) -> &str {
        &self.code_marker
    }
    fn link(&self) -> Style {
        Style::default()
            .fg(crate::ui::theme::MARKDOWN_LINK)
            .add_modifier(Modifier::UNDERLINED)
            .underline_color(self.links.next())
    }
}
/// Display-only tab stops. Keep immutable source, copy values, and span styles.
pub(crate) fn expand_tabs(line: &mut Line<'_>) {
    use unicode_width::UnicodeWidthStr;
    if !line.spans.iter().any(|span| span.content.contains('\t')) {
        return;
    }
    let mut column = 0usize;
    for span in &mut line.spans {
        if !span.content.contains('\t') {
            column = column.saturating_add(span.content.width());
            continue;
        }
        let mut expanded = String::with_capacity(span.content.len());
        for part in span.content.split_inclusive('\t') {
            let body = part.strip_suffix('\t').unwrap_or(part);
            expanded.push_str(body);
            column = column.saturating_add(body.width());
            if part.ends_with('\t') {
                let spaces = 4 - column % 4;
                expanded.extend(std::iter::repeat_n(' ', spaces));
                column = column.saturating_add(spaces);
            }
        }
        span.content = Cow::Owned(expanded);
    }
}

// Ephemeral parser facts for rendering and copy; source offsets stay in bytes.
pub(crate) struct CodeBlockSource {
    pub text: String,
    end: usize,
    content_end: usize,
}
impl CodeBlockSource {
    pub fn is_complete(&self, source_len: usize, source_complete: bool) -> bool {
        source_complete || self.end < source_len || self.end > self.content_end
    }
}

pub(crate) fn markdown_code_blocks(source: &str) -> impl Iterator<Item = CodeBlockSource> + '_ {
    let mut body = None;
    let mut content_end = 0;
    Parser::new_ext(source, markdown_options())
        .into_offset_iter()
        .filter_map(move |(event, range)| match event {
            Event::Start(Tag::CodeBlock(_)) => {
                body = Some(String::new());
                // For an empty fenced block, the opener itself is not evidence
                // of a closing delimiter beyond the final content position.
                content_end = source[range.start..range.end]
                    .find('\n')
                    .map_or(range.end, |at| range.start + at + 1);
                None
            }
            Event::Text(text) => {
                if let Some(body) = &mut body {
                    body.push_str(&text);
                    content_end = content_end.max(range.end);
                }
                None
            }
            Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                body.take().map(|text| CodeBlockSource {
                    text,
                    end: range.end,
                    content_end,
                })
            }
            _ => None,
        })
}

// This annotates OSS-generated code regions; Markdown layout and token highlighting stay in OSS.
fn style_code_regions(
    lines: Vec<Line<'static>>,
    marker: &str,
    source: &str,
    width: u16,
) -> Vec<Line<'static>> {
    fn contents(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }
    fn source_lines(text: &str) -> Vec<String> {
        let text = text.replace("\r\n", "\n");
        let mut lines: Vec<_> = text.split('\n').map(str::to_owned).collect();
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }
    let mut sources = markdown_code_blocks(source).map(|block| block.text);
    let mut output = Vec::new();
    let mut code: Option<(String, Vec<Line<'static>>)> = None;
    let background = crate::ui::theme::CODE_BACKGROUND;
    for line in lines {
        let content = contents(&line);
        if let Some(at) = content.find(marker) {
            if let Some((language, mut body)) = code.take() {
                let original = sources.next().unwrap_or_default();
                let expected = source_lines(&original)
                    .into_iter()
                    .map(|text| {
                        let mut line = Line::from(text);
                        expand_tabs(&mut line);
                        contents(&line)
                    })
                    .collect::<Vec<_>>();
                for line in &mut body {
                    expand_tabs(line);
                }
                if body.iter().map(contents).collect::<Vec<_>>() != expected {
                    // Upstream highlighter/conversion failure must never silently omit source lines.
                    body = expected
                        .into_iter()
                        .map(|text| {
                            Line::from(Span::styled(
                                text,
                                Style::default().fg(crate::ui::theme::CODE_TEXT),
                            ))
                        })
                        .collect();
                }
                if !matches!(language.as_str(), "" | "text" | "plaintext" | "plain") {
                    let mut caption = Line::from(Span::styled(
                        language.clone(),
                        Style::default().fg(crate::ui::theme::CODE_CAPTION),
                    ));
                    let padding = usize::from(width).saturating_sub(caption.width());
                    caption.spans.insert(0, Span::raw(" ".repeat(padding)));
                    output.push(caption.style(Style::default().bg(background)));
                }
                for mut line in body {
                    line.spans.insert(0, Span::raw("  "));
                    let pad = usize::from(width).saturating_sub(line.width());
                    if pad > 0 {
                        line.spans.push(Span::raw(" ".repeat(pad)));
                    }
                    // Plain/unknown-language spans may inherit the terminal foreground,
                    // which can be dark on a light theme. Keep the code surface paired,
                    // while preserving explicit colors produced by the OSS highlighter.
                    line.style = line.style.fg(crate::ui::theme::CODE_TEXT).bg(background);
                    for span in &mut line.spans {
                        if span
                            .style
                            .fg
                            .is_none_or(|color| color == ratatui::style::Color::Reset)
                        {
                            span.style = span.style.fg(crate::ui::theme::CODE_TEXT);
                        }
                    }
                    output.push(line);
                }
                output.push(
                    Line::from(" ".repeat(usize::from(width)))
                        .style(Style::default().bg(background)),
                );
            } else {
                code = Some((content[at + marker.len()..].to_owned(), Vec::new()));
            }
        } else if let Some((_, body)) = &mut code {
            body.push(line);
        } else {
            output.push(line);
        }
    }
    output
}
/// Run only on a bounded presentation worker, including when width changes.
/// Markdown syntax/layout is delegated to tui-markdown; parser tokens remain source/link metadata.
pub fn render_markdown(presentation: &mut NodePresentation, width: u16) {
    for block in &mut presentation.blocks {
        if !matches!(block.kind, BlockKind::Prose | BlockKind::Reasoning) {
            block.rendered = None;
            continue;
        }
        let mut marker = format!(
            "\u{e000}shellboy-code-fence-{}\u{e001}",
            uuid::Uuid::new_v4()
        );
        while block.text.contains(&marker) {
            marker = format!(
                "\u{e000}shellboy-code-fence-{}\u{e001}",
                uuid::Uuid::new_v4()
            );
        }
        let links = std::sync::Arc::new(markdown_links::LinkMarkers::new(
            &block.text,
            &presentation.links,
        ));
        let options = tui_markdown::Options::new(ShellboyMarkdownStyle {
            code_marker: marker.clone(),
            links: links.clone(),
        })
        .table_width(width);
        // highlight-code uses tui-markdown's shared bundled Base16OceanDark theme.
        let text = tui_markdown::from_str_with_options(&block.text, &options);
        let lines: Vec<Line<'static>> = text
            .lines
            .into_iter()
            .map(|line| Line {
                style: line.style,
                alignment: line.alignment,
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span {
                        style: span.style,
                        content: Cow::Owned(span.content.into_owned()),
                    })
                    .collect(),
            })
            .collect();
        let mut lines = style_code_regions(lines, &marker, &block.text, width);
        for line in &mut lines {
            expand_tabs(line);
        }
        if !links.complete() {
            // An OSS callback contract change must not silently bind the wrong URL.
            for span in lines.iter_mut().flat_map(|line| &mut line.spans) {
                span.style.underline_color = Some(ratatui::style::Color::Reset);
            }
        }
        block.rendered = Some(RenderedMarkdown {
            width,
            lines,
            omitted: false,
        });
    }
    presentation.bound_metadata();
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationField {
    #[serde(default)]
    pub value_omitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub key: String,
    pub label: String,
    pub value: String,
}
impl PresentationField {
    pub fn displayed(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.value)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationLink {
    pub label: String,
    pub url: String,
    pub source_start: usize,
    pub source_end: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventClass {
    #[default]
    Unknown,
    Routine,
    Change,
    Attention,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputSource {
    Human,
    Parent,
    SelfModel,
    OtherModel,
    Revisit,
    #[default]
    Unknown,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePresentation {
    #[serde(skip)]
    pub created_at: Option<String>,
    #[serde(skip)]
    pub input_source: InputSource,
    /// Worker-derived display identity; never persisted or a source/provenance hash.
    #[serde(skip)]
    pub projection_stamp: Option<[u8; 32]>,
    /// Ephemeral cache state, distinct from a legitimately empty source body.
    #[serde(skip)]
    pub body_evicted: bool,
    /// Serialized DB-read ordering for this source window; not semantic identity.
    #[serde(skip)]
    pub read_revision: u64,
    #[serde(skip)]
    pub provisional_stream: bool,
    #[serde(default)]
    pub event_class: EventClass,
    pub kind: PresentationKind,
    pub title: String,
    pub blocks: Vec<PresentationBlock>,
    pub fields: Vec<PresentationField>,
    pub links: Vec<PresentationLink>,
    pub details_omitted: bool,
    #[serde(default)]
    pub metadata_limited: bool,
    #[serde(default)]
    pub body_omitted_chars: usize,
    #[serde(default)]
    pub body_extent_unknown: bool,
}
impl NodePresentation {
    pub fn requires_attention(&self) -> bool {
        self.event_class == EventClass::Attention
            || self.fields.iter().any(|f| {
                ((f.key.ends_with("/state") || f.key.ends_with("/status"))
                    && matches!(
                        f.value.as_str(),
                        "failed" | "stopped" | "cancelled" | "recovery_wait"
                    ))
                    || (f.key.ends_with("/exit_code")
                        && f.value.parse::<i64>().is_ok_and(|c| c != 0))
                    || (f.key.ends_with("/output_complete") && f.value == "false")
                    || f.key.contains("/error/")
                    || (f.key.ends_with("/error") && f.value != "null")
                    || f.key.ends_with("/hint")
            })
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    /// Metadata has its own small bound; text remains charged to the caller's body cache.
    pub fn metadata_bytes(&self) -> usize {
        self.retained_bytes()
            .saturating_sub(self.blocks.iter().map(|b| b.text.capacity()).sum::<usize>())
    }
    pub(crate) fn bound_metadata(&mut self) {
        if self.metadata_bytes() <= MAX_METADATA_BYTES {
            return;
        }
        self.details_omitted = true;
        self.metadata_limited = true;
        for i in (0..self.blocks.len()).rev() {
            if self.metadata_bytes() <= MAX_METADATA_BYTES {
                break;
            }
            if let Some(rendered) = &mut self.blocks[i].rendered {
                rendered.lines = Vec::new();
                rendered.omitted = true;
            }
        }
        for i in (0..self.blocks.len()).rev() {
            if self.metadata_bytes() <= MAX_METADATA_BYTES {
                break;
            }
            self.blocks[i].markdown = Vec::new();
        }
        while self.metadata_bytes() > MAX_METADATA_BYTES && !self.fields.is_empty() {
            self.fields.pop();
            self.fields.shrink_to_fit();
        }
        while self.metadata_bytes() > MAX_METADATA_BYTES && !self.links.is_empty() {
            self.links.pop();
            self.links.shrink_to_fit();
        }
        for i in (0..self.blocks.len()).rev() {
            if self.metadata_bytes() <= MAX_METADATA_BYTES {
                break;
            }
            self.blocks[i].tool = None;
        }
    }
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.created_at.as_ref().map_or(0, String::capacity)
            + self.title.capacity()
            + self.blocks.capacity() * size_of::<PresentationBlock>()
            + self.fields.capacity() * size_of::<PresentationField>()
            + self.links.capacity() * size_of::<PresentationLink>()
            + self
                .fields
                .iter()
                .map(|f| {
                    f.key.capacity()
                        + f.label.capacity()
                        + f.value.capacity()
                        + f.display.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
            + self
                .links
                .iter()
                .map(|l| l.label.capacity() + l.url.capacity())
                .sum::<usize>()
            + self
                .blocks
                .iter()
                .map(|b| {
                    b.text.capacity()
                        + b.tool.as_ref().map_or(0, ToolPresentation::retained_bytes)
                        + b.rendered
                            .as_ref()
                            .map_or(0, RenderedMarkdown::retained_bytes)
                        + b.markdown.capacity() * size_of::<MarkdownToken>()
                        + b.markdown
                            .iter()
                            .map(|t| {
                                t.text.capacity()
                                    + match &t.kind {
                                        MarkdownKind::CodeBlock(s) | MarkdownKind::Link(s) => {
                                            s.capacity()
                                        }
                                        _ => 0,
                                    }
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}
fn secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "encrypted_content",
        "encryptedcontent",
        "origin_token",
        "authorization",
        "api_key",
        "api-key",
        "apikey",
        "secret",
        "password",
        "access_token",
        "refresh_token",
    ]
    .iter()
    .any(|s| key.contains(s))
        || key == "token"
}
fn short(text: &str, limit: usize) -> String {
    let mut out: String = text.chars().take(limit).collect();
    if out.len() < text.len() {
        out.push_str("… [詳細は原文参照]");
    }
    out
}
// Persisted envelope values may be scalar JSON. Detail decoding intentionally
// accepts only containers and trims before checking their shape.
fn parse_envelope(text: &str) -> Option<Value> {
    serde_json::from_str(text.strip_prefix("Shellboy event v1: ").unwrap_or(text)).ok()
}
fn parse_container_detail(text: &str) -> Option<Value> {
    let body = text
        .strip_prefix("Shellboy event v1: ")
        .unwrap_or(text)
        .trim();
    if body.starts_with('{') || body.starts_with('[') {
        serde_json::from_str(body).ok()
    } else {
        None
    }
}

/// Remove opaque/credential fields for explicit diagnostic pages as well as summaries.
pub fn safe_detail_value(value: Value) -> Value {
    fn safe(value: Value, depth: usize) -> Value {
        if depth > 32 {
            return Value::String("[深い構造 · 詳細は元の参照先で確認]".into());
        }
        match value {
            Value::Object(object) => Value::Object(
                object
                    .into_iter()
                    .filter(|(key, _)| !secret(key))
                    .map(|(key, value)| (key, safe(value, depth + 1)))
                    .collect(),
            ),
            Value::Array(values) => {
                Value::Array(values.into_iter().map(|v| safe(v, depth + 1)).collect())
            }
            Value::String(text) => {
                if let Some(parsed) = parse_container_detail(&text) {
                    safe(parsed, depth + 1)
                } else {
                    Value::String(sanitize_plain(&text, 0).0)
                }
            }
            other => other,
        }
    }
    safe(value, 0)
}
pub use fields::fields_of_value;
fn add_link(out: &mut Vec<PresentationLink>, label: &str, url: &str, start: usize, end: usize) {
    if http(url)
        && out.len() < MAX_LINKS
        && !out.iter().any(|l| l.url == url && l.source_start == start)
        && out
            .iter()
            .map(|l| l.url.len() + l.label.len())
            .sum::<usize>()
            + url.len()
            + label.len()
            <= MAX_METADATA_BYTES
    {
        out.push(PresentationLink {
            label: label.into(),
            url: url.into(),
            source_start: start,
            source_end: end,
        });
    }
}
// A broken string stops at its next line or escape, so retrying that escape
// cannot repeatedly scan the remaining input. Only a complete OSC owns payload.
fn osc_control_end(chars: &[char], mut at: usize) -> Option<(usize, usize)> {
    while let Some(&c) = chars.get(at) {
        match c {
            '\u{7}' => return Some((at, at + 1)),
            '\u{1b}' if chars.get(at + 1) == Some(&'\\') => return Some((at, at + 2)),
            '\n' | '\r' | '\u{1b}' => return None,
            _ => at += 1,
        }
    }
    None
}

fn csi_control_end(chars: &[char], mut at: usize) -> Option<usize> {
    let mut intermediate = false;
    while let Some(&c) = chars.get(at) {
        match c {
            ' '..='/' => intermediate = true,
            '0'..='?' if !intermediate => {}
            '@'..='~' => return Some(at + 1),
            _ => return None,
        }
        at += 1;
    }
    None
}

/// Strip terminal control sequences; OSC 8 URLs become metadata, never terminal bytes.
pub fn sanitize_plain(text: &str, base: usize) -> (String, Vec<PresentationLink>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut links = vec![];
    let mut i = 0;
    let mut open: Option<(String, usize, usize)> = None;
    let mut controls = Vec::new();
    while i < chars.len() {
        if chars[i] == '\u{1b}' {
            let control_start = i;
            if chars.get(i + 1) == Some(&']') {
                let start = i;
                let begin = i + 2;
                if let Some((end, sequence_end)) = osc_control_end(&chars, begin) {
                    let payload: String = chars[begin..end].iter().collect();
                    if let Some(rest) = payload.strip_prefix("8;")
                        && let Some((_, url)) = rest.split_once(';')
                    {
                        if url.is_empty() {
                            if let Some((url, at, label_byte)) = open.take() {
                                add_link(
                                    &mut links,
                                    &out[label_byte..],
                                    &url,
                                    base + at,
                                    base + start,
                                );
                            }
                        } else if http(url) {
                            open = Some((url.into(), sequence_end, out.len()));
                        }
                    }
                    i = sequence_end;
                } else {
                    // Drop the unsafe introducer, then preserve its payload as
                    // ordinary sanitized text instead of hiding the remainder.
                    i = begin;
                }
            } else if chars.get(i + 1) == Some(&'[') {
                i = csi_control_end(&chars, i + 2).unwrap_or(i + 2);
            } else {
                i = (i + 2).min(chars.len());
            }
            controls.push((control_start, i));
        } else {
            let c = chars[i];
            if c == '\n' || c == '\t' || (!c.is_control() && c != '\u{7f}') {
                out.push(c);
            }
            i += 1;
        }
    }
    if let Some((url, at, label_byte)) = open {
        add_link(
            &mut links,
            &out[label_byte..],
            &url,
            base + at,
            base + chars.len(),
        );
    }
    // Bare URL offsets refer to the unmodified input. Recognition performs no I/O.
    for (at, _) in text.match_indices("http") {
        if links.len() >= MAX_LINKS {
            break;
        }
        if !text[at..].starts_with("http://") && !text[at..].starts_with("https://") {
            continue;
        }
        let raw = text[at..]
            .split(|c: char| {
                c.is_whitespace() || c.is_control() || ['<', '>', '"', '\'', '`'].contains(&c)
            })
            .next()
            .unwrap_or("");
        let mut url = raw.trim_end_matches([
            '.', ',', ';', ':', '。', '、', '！', '？', '）', '】', '」', '』',
        ]);
        for (left, right) in [('(', ')'), ('[', ']'), ('{', '}')] {
            while url.ends_with(right) && url.matches(right).count() > url.matches(left).count() {
                url = url.strip_suffix(right).unwrap();
            }
        }
        if http(url) {
            let local = text[..at].chars().count();
            let seq = controls.partition_point(|&(_, end)| end <= local);
            if controls
                .get(seq)
                .is_some_and(|&(begin, end)| local >= begin && local < end)
            {
                continue;
            }
            let start = base + local;
            add_link(&mut links, url, url, start, start + url.chars().count());
        }
    }
    (out, links)
}
fn markdown_options() -> Options {
    // Keep in lockstep with tui-markdown 0.3.12 renderer/mod.rs.
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_MATH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_GFM
}
fn markdown(text: &str, base: usize, links: &mut Vec<PresentationLink>) -> Vec<MarkdownToken> {
    let mut tokens = Vec::new();
    let mut bytes = 0;
    let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let options = markdown_options();
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        let start = base + boundaries.partition_point(|&at| at < range.start);
        let end = base + boundaries.partition_point(|&at| at < range.end);
        let (kind, body) = match event {
            Event::Start(tag) => (
                match tag {
                    Tag::Heading { level, .. } => MarkdownKind::Heading(level as u8),
                    Tag::Paragraph => MarkdownKind::Paragraph,
                    Tag::List(start) => MarkdownKind::List(start),
                    Tag::Item => MarkdownKind::ListItem,
                    Tag::BlockQuote(_) => MarkdownKind::Quote,
                    Tag::CodeBlock(kind) => MarkdownKind::CodeBlock(match kind {
                        CodeBlockKind::Fenced(s) => s.into_string(),
                        CodeBlockKind::Indented => String::new(),
                    }),
                    Tag::Strong => MarkdownKind::Strong,
                    Tag::Emphasis => MarkdownKind::Emphasis,
                    Tag::Link { dest_url, .. } => {
                        let url = dest_url.into_string();
                        if http(&url) {
                            let raw = &text[range.clone()];
                            let label = raw
                                .strip_prefix('[')
                                .and_then(|s| s.split_once(']').map(|p| p.0))
                                .unwrap_or(raw);
                            links.retain(|l| !(l.source_start >= start && l.source_start < end));
                            add_link(links, label, &url, start, end);
                            MarkdownKind::Link(url)
                        } else {
                            MarkdownKind::Paragraph
                        }
                    }
                    Tag::TableRow | Tag::TableHead => MarkdownKind::TableRow,
                    Tag::TableCell => MarkdownKind::TableCell,
                    _ => MarkdownKind::Paragraph,
                },
                String::new(),
            ),
            Event::End(_) => (MarkdownKind::End, String::new()),
            Event::Text(s) => (MarkdownKind::Text, s.into_string()),
            Event::Code(s) => (MarkdownKind::Code, s.into_string()),
            Event::Html(s) | Event::InlineHtml(s) => (MarkdownKind::Text, s.into_string()),
            Event::SoftBreak => (MarkdownKind::SoftBreak, String::new()),
            Event::HardBreak => (MarkdownKind::HardBreak, String::new()),
            Event::Rule => (MarkdownKind::Rule, String::new()),
            Event::TaskListMarker(checked) => (
                MarkdownKind::Text,
                if checked { "[x] " } else { "[ ] " }.into(),
            ),
            _ => (MarkdownKind::Text, text[range.clone()].into()),
        };
        let body = sanitize_plain(&body, start).0;
        bytes += size_of::<MarkdownToken>() + body.len();
        if bytes > MAX_METADATA_BYTES {
            return vec![];
        }
        tokens.push(MarkdownToken {
            kind,
            text: body,
            source_start: start,
            source_end: end,
        });
    }
    tokens
}
fn excerpt(text: &str, limit: usize, tail: bool) -> (String, usize) {
    if text.len() <= limit {
        return (text.into(), 0);
    }
    if tail {
        let at = text.ceil_char_boundary(text.len().saturating_sub(limit));
        (text[at..].to_owned(), text[..at].chars().count())
    } else {
        let at = text.floor_char_boundary(limit.min(text.len()));
        (text[..at].to_owned(), text[at..].chars().count())
    }
}
/// A transient attempt's contiguous text window. Positions belong to the stream,
/// not to the eventual canonical response, whose item ordering/text may differ.
pub(crate) fn project_stream_window(
    text: &str,
    base: usize,
    reasoning: &[(usize, usize)],
    width: u16,
) -> NodePresentation {
    fn segment(p: &mut NodePresentation, text: &str, start: usize, end: usize, reasoning: bool) {
        if text.is_empty() {
            return;
        }
        if p.blocks.len() >= MAX_BLOCKS {
            p.body_omitted_chars += end - start;
            return;
        }
        let (safe, links) = sanitize_plain(text, start);
        for link in links {
            add_link(
                &mut p.links,
                &link.label,
                &link.url,
                link.source_start,
                link.source_end,
            );
        }
        let tokens = markdown(text, start, &mut p.links);
        p.blocks.push(PresentationBlock {
            kind: if reasoning {
                BlockKind::Reasoning
            } else {
                BlockKind::Prose
            },
            source_exact: safe == text,
            text: safe,
            source_start: start,
            source_end: end,
            markdown: tokens,
            ..Default::default()
        });
    }
    let mut p = NodePresentation {
        kind: PresentationKind::Assistant,
        provisional_stream: true,
        ..Default::default()
    };
    let mut range = 0;
    let mut begin_byte = 0;
    let mut begin_char = base;
    let mut position = base;
    let mut previous = false;
    for (byte, _) in text.char_indices() {
        while range < reasoning.len() && reasoning[range].1 <= position {
            range += 1;
        }
        let current = reasoning
            .get(range)
            .is_some_and(|(start, end)| *start <= position && position < *end);
        if byte == 0 {
            previous = current;
        }
        if current != previous {
            segment(
                &mut p,
                &text[begin_byte..byte],
                begin_char,
                position,
                previous,
            );
            begin_byte = byte;
            begin_char = position;
            previous = current;
        }
        position += 1;
    }
    segment(&mut p, &text[begin_byte..], begin_char, position, previous);
    render_markdown(&mut p, width.max(1));
    p
}

#[cfg(test)]
mod stream_projection_tests {
    use super::*;
    #[test]
    fn provisional_unicode_ranges_are_projected_without_mutating_the_raw_window() {
        let raw = "考え\n```rust\nlet\tx = 1;\n```\n答え";
        let split = "考え".chars().count();
        let p = project_stream_window(raw, 100, &[(100, 100 + split)], 35);
        assert_eq!(p.kind, PresentationKind::Assistant);
        assert_eq!(p.blocks[0].kind, BlockKind::Reasoning);
        assert_eq!(
            (p.blocks[0].source_start, p.blocks[0].source_end),
            (100, 102)
        );
        assert_eq!(p.blocks[1].kind, BlockKind::Prose);
        assert_eq!(p.blocks[1].source_end, 100 + raw.chars().count());
        assert!(p.blocks[1].rendered.is_some());
        assert!(p.metadata_bytes() <= MAX_METADATA_BYTES);
        assert!(raw.contains('\t'));
    }
}
