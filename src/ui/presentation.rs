//! Display-only layout of worker-produced typed blocks and Markdown tokens.
use super::history_items::{ItemKey, ItemRegion};
use super::theme;
use super::{ThreadView, UiNode, rendered_node_text};
use crate::config::UiLanguage;
use crate::presentation::messages::{Message, Text, row_label};
use crate::presentation::rows::{RowGroup, RowKind, blank_between};
use crate::presentation::{BlockKind, OutputStream, PresentationKind};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

fn conversational(node: &UiNode) -> bool {
    matches!(
        node.presentation.kind,
        PresentationKind::User | PresentationKind::Assistant
    ) || (node.presentation.kind == PresentationKind::Unknown
        && node.presentation.title.is_empty()
        && matches!(node.role.as_str(), "user" | "assistant"))
}
fn source_candidate(text: &str) -> &str {
    text.trim_start_matches([' ', '›', '▸', '│', '┃']).trim()
}

fn saved_failure_text(node: &UiNode, language: UiLanguage) -> Option<(String, String)> {
    if node.presentation.kind != PresentationKind::Event {
        return None;
    }
    let field = |key: &str| {
        node.presentation
            .fields
            .iter()
            .find(|f| f.key == key && !f.value_omitted)
    };
    let count = field("/saved_model_failures/count")?
        .value
        .parse::<u64>()
        .ok()?;
    let title = match (language, count) {
        (UiLanguage::En, 1) => "Recovery resumed | 1 model failure recorded before resuming".into(),
        (UiLanguage::En, _) => {
            format!("Recovery resumed | {count} model failures recorded before resuming")
        }
        (UiLanguage::Ja, _) => {
            format!("復旧再開 | 再開前にこの応答処理で記録したモデル失敗{count}件")
        }
    };
    let body = match (language, field("/saved_model_failures/error/message")) {
        (UiLanguage::En, Some(reason)) => format!("Last saved reason: {}", reason.value),
        (UiLanguage::Ja, Some(reason)) => format!("直近の保存理由: {}", reason.value),
        (UiLanguage::En, None) => "Read the saved record for the latest reason | F6 Details".into(),
        (UiLanguage::Ja, None) => "直近の理由は保存記録で確認してください | F6 詳細".into(),
    };
    Some((title, body))
}

fn label(language: UiLanguage, node: &UiNode) -> String {
    if language == UiLanguage::En
        && let Some((title, _)) = saved_failure_text(node, language)
    {
        return title;
    }
    if language == UiLanguage::En
        && let Some(message) = &node.presentation.ui_messages.title
        && (node.presentation.ui_messages.notice.is_some()
            || !node
                .presentation
                .fields
                .iter()
                .any(|f| f.key == "/ui_notice/title"))
    {
        let title = message.render(language);
        return match &node.presentation.ui_messages.notice {
            Some(notice) => format!("{title} | {}", notice.render(language)),
            None => title,
        };
    }
    if let Some(title) = node
        .presentation
        .fields
        .iter()
        .find(|f| f.key == "/ui_notice/title")
    {
        return title.value.clone();
    }
    if node.presentation.kind == PresentationKind::Event {
        if let Some(fact) = node
            .presentation
            .title
            .strip_prefix("状態変更 · ")
            .or_else(|| node.presentation.title.strip_prefix("状態変更 | "))
        {
            return fact.into();
        }
        if node.presentation.title == "状態変更" {
            return Text::ReviewChange.render(language).into();
        }
    }
    if !node.presentation.title.is_empty() {
        return node.presentation.title.clone();
    }
    match node.role.as_str() {
        "user" => Text::User.render(language),
        "assistant" => "Shellboy",
        "developer" | "system" => Text::Event.render(language),
        "tool" => Text::ToolResult.render(language),
        _ => Text::History.render(language),
    }
    .into()
}
fn color(node: &UiNode) -> Color {
    if node.presentation.requires_attention()
        || node.presentation.fields.iter().any(|f| {
            matches!(
                f.value.as_str(),
                "failed" | "error" | "recovery_wait" | "cancelled" | "stopped"
            )
        })
    {
        return theme::ATTENTION;
    }
    match node.presentation.kind {
        PresentationKind::User => theme::EMPHASIS,
        PresentationKind::Assistant => theme::TEXT,
        PresentationKind::Event => RowKind::Notice.spec().2,
        PresentationKind::ToolCall | PresentationKind::ToolResult => theme::TOOL,
        PresentationKind::Interrupted => theme::ATTENTION,
        PresentationKind::Summary | PresentationKind::Spill => theme::REPLACEMENT,
        PresentationKind::Unknown => theme::TEXT,
    }
}
// Private layout metadata in the existing marker channel. Link identities are
// RGB; this non-RGB value is ignored by hit-testing and stripped by borrowed().
const EXCERPT_GAP_MARKER: Color = Color::Indexed(255);
const OUTPUT_ROW_MARKER: Color = Color::Indexed(254);
const INPUT_DECORATION_MARKER: Color = Color::Indexed(253);

fn body_spans<'a>(line: &'a Line<'_>) -> impl Iterator<Item = &'a Span<'a>> {
    line.spans.iter().filter(|span| {
        !matches!(
            span.style.underline_color,
            Some(INPUT_DECORATION_MARKER | OUTPUT_ROW_MARKER | theme::COPY_DECORATION)
        )
    })
}

#[derive(Clone, Copy)]
enum LineRole {
    Meta,
    Body,
    Reasoning,
    Tool,
}
struct NodeLines {
    role: LineRole,
    rows: Vec<(LineRole, Line<'static>)>,
}
impl NodeLines {
    fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            role: LineRole::Meta,
            rows: lines
                .into_iter()
                .map(|line| (LineRole::Meta, line))
                .collect(),
        }
    }
    fn push(&mut self, mut line: Line<'static>) {
        crate::presentation::expand_tabs(&mut line);
        self.rows.push((self.role, line));
    }
    fn extend(&mut self, lines: impl IntoIterator<Item = Line<'static>>) {
        for line in lines {
            self.push(line);
        }
    }
}
fn node_lines_inner(
    language: UiLanguage,
    node: &UiNode,
    expanded: bool,
    start: usize,
    width: u16,
    targets: &mut Vec<std::sync::Arc<str>>,
    show_header: bool,
) -> NodeLines {
    let conversational = conversational(node);
    let typed_tools = node
        .presentation
        .blocks
        .iter()
        .any(|block| block.tool.is_some());
    let header = if !show_header
        || conversational
        || node.presentation.kind == PresentationKind::ToolCall
        || typed_tools
    {
        Vec::new()
    } else {
        vec![Line::from(Span::styled(
            if node.presentation.kind == PresentationKind::ToolResult {
                format!("{} {}", RowKind::ToolCall.mark(), label(language, node))
            } else {
                match node.presentation.kind {
                    PresentationKind::Event => crate::ui::text_width::glyphs::formatted!(
                        "{} {} · {}",
                        "{} {} | {}",
                        RowKind::Notice.mark(),
                        row_label(RowKind::Notice, language),
                        label(language, node)
                    ),
                    PresentationKind::Summary => {
                        format!("{} {}", RowKind::Summary.mark(), label(language, node))
                    }
                    PresentationKind::Spill => {
                        format!("{} {}", RowKind::Spill.mark(), label(language, node))
                    }
                    PresentationKind::Interrupted => {
                        format!("{} {}", RowKind::Cancelled.mark(), label(language, node))
                    }
                    _ => label(language, node).to_owned(),
                }
            },
            Style::default()
                .fg(color(node))
                .add_modifier(Modifier::BOLD),
        ))]
    };
    let mut out = NodeLines::new(header);
    if node.presentation.body_evicted || (node.text.is_empty() && node.total_chars > 0) {
        out.role = LineRole::Body;
        out.push(Line::from(Text::Uncached.render(language)));
        return out;
    }
    if node.presentation.blocks.is_empty() {
        out.role = LineRole::Body;
        if matches!(node.role.as_str(), "developer" | "system" | "tool") {
            out.push(Line::from(Span::styled(
                Text::Details.render(language),
                Style::default().fg(theme::MUTED),
            )));
        } else {
            out.extend(
                crate::presentation::sanitize_plain(
                    &rendered_node_text(node, expanded, start, usize::MAX),
                    start,
                )
                .0
                .lines()
                .map(|line| Line::from(line.to_owned())),
            );
        }
        return out;
    }
    if node.presentation.kind == PresentationKind::Unknown {
        return out;
    }
    let mut tool_result_seen = false;
    let mut previous_tool = None;
    for (block_index, block) in node.presentation.blocks.iter().enumerate() {
        out.role = match block.kind {
            BlockKind::Reasoning => LineRole::Reasoning,
            BlockKind::ToolCall | BlockKind::ToolResult => LineRole::Tool,
            BlockKind::Prose | BlockKind::Plain | BlockKind::Code => LineRole::Body,
            BlockKind::Event => LineRole::Meta,
        };
        if block.source_end <= start && block.source_end > block.source_start {
            continue;
        }
        if block.kind == BlockKind::Reasoning && !expanded {
            out.push(Line::from(Span::styled(
                Message::Reasoning {
                    count: block.source_end.saturating_sub(block.source_start),
                }
                .render(language),
                Style::default().fg(theme::MUTED),
            )));
            continue;
        }
        if block.kind == BlockKind::Reasoning {
            out.push(Line::from(Span::styled(
                Text::ReasoningExpanded.render(language),
                Style::default().fg(theme::MUTED),
            )));
        }
        if block.kind == BlockKind::Event && block.text == node.presentation.title {
            continue;
        }
        if let Some(tool) = &block.tool {
            if previous_tool.as_deref() != Some(tool.call_id.as_str()) {
                out.extend(tool_header_lines(
                    language,
                    tool,
                    block.kind == BlockKind::ToolCall,
                    width,
                    &node.presentation.links,
                    targets,
                ));
                if !tool.reason.is_empty() {
                    out.extend(hanging_wrap(
                        vec![Line::from(Span::styled(
                            crate::ui::text_width::glyphs::formatted!(
                                "  ┊ {}",
                                "  | {}",
                                crate::presentation::messages::tool_reason(tool, language)
                            ),
                            Style::default().fg(theme::MUTED),
                        ))],
                        width,
                    ));
                }
            }
            previous_tool = Some(tool.call_id.clone());
            // Commands live in the call heading; do not print them a second time.
            if block.kind == BlockKind::ToolCall
                || matches!(block.text.as_str(), "実行結果 | 詳細" | "実行結果 · 詳細")
            {
                continue;
            }
        } else {
            previous_tool = None;
        }
        if let Some(rendered) = block.rendered.as_ref().filter(|rendered| {
            rendered.width == width
                && !rendered.omitted
                && (!block.source_exact || start <= block.source_start)
        }) {
            let mut lines = rendered.lines.clone();
            for span in lines.iter_mut().flat_map(|line| &mut line.spans) {
                if let Some(marker) = span.style.underline_color {
                    let index =
                        node.presentation
                            .links
                            .iter()
                            .enumerate()
                            .find_map(|(index, link)| {
                                (marker == crate::terminal_links::marker_color(index))
                                    .then_some(link)
                            });
                    span.style.underline_color = Some(index.map_or(Color::Reset, |link| {
                        crate::terminal_links::marker_color(link_target(targets, &link.url))
                    }));
                }
            }
            out.extend(lines);
        } else {
            let text = if block.source_exact
                && start > block.source_start
                && block.source_end - block.source_start == block.text.chars().count()
            {
                block
                    .text
                    .chars()
                    .skip(start - block.source_start)
                    .collect::<String>()
            } else {
                if language == UiLanguage::En
                    && block.kind == BlockKind::Event
                    && let Some((_, body)) = saved_failure_text(node, language)
                {
                    body
                } else {
                    node.presentation
                        .ui_messages
                        .bodies
                        .iter()
                        .find(|(index, _)| {
                            language == UiLanguage::En
                                && block.kind == BlockKind::Event
                                && !block.source_exact
                                && *index == block_index
                        })
                        .map(|(_, messages)| {
                            messages
                                .iter()
                                .map(|m| m.render(language))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_else(|| block.text.clone())
                }
            };
            if block.kind == BlockKind::ToolCall {
                let mut command = text
                    .lines()
                    .map(|line| {
                        Line::from(Span::styled(
                            line.to_owned(),
                            Style::default().fg(theme::TOOL),
                        ))
                    })
                    .collect::<Vec<_>>();
                annotate_links(&mut command, &node.presentation.links, targets);
                let mut command = hanging_wrap(command, width.saturating_sub(9).max(1));
                for (index, line) in command.iter_mut().enumerate() {
                    line.spans.insert(
                        0,
                        Span::styled(
                            if index == 0 {
                                format!("{} shell  ", RowKind::ToolCall.mark())
                            } else {
                                "         ".into()
                            },
                            Style::default().fg(theme::MUTED),
                        ),
                    );
                }
                out.extend(command);
            } else if block.kind == BlockKind::ToolResult {
                if tool_result_seen {
                    out.push(Line::default());
                }
                tool_result_seen = true;
                let guide_color = if block.stream == Some(OutputStream::Stderr) {
                    theme::ATTENTION
                } else {
                    theme::MUTED
                };
                let mut output = text
                    .lines()
                    .map(|line| {
                        Line::from(Span::styled(
                            line.to_owned(),
                            Style::default().fg(theme::TEXT),
                        ))
                    })
                    .collect::<Vec<_>>();
                annotate_links(&mut output, &node.presentation.links, targets);
                let mut output = hanging_wrap(output, width.saturating_sub(2).max(1));
                let count = output.len();
                if count > 8 {
                    let tail = output.split_off(count - 2);
                    output.truncate(3);
                    output.push(Line::from(Span::styled(
                        Message::Excerpt {
                            rows: count,
                            omitted: count - 5,
                        }
                        .render(language),
                        Style::default()
                            .fg(theme::MUTED)
                            .underline_color(EXCERPT_GAP_MARKER),
                    )));
                    output.extend(tail);
                }
                output = hanging_wrap(output, width.saturating_sub(2).max(1));
                for line in &mut output {
                    let marker = if line
                        .spans
                        .iter()
                        .any(|span| span.style.underline_color == Some(EXCERPT_GAP_MARKER))
                    {
                        EXCERPT_GAP_MARKER
                    } else {
                        OUTPUT_ROW_MARKER
                    };
                    // The guide owns the row tag; keep every actual URL's RGB
                    // marker and unavailable-target marker exactly as supplied.
                    line.spans.insert(
                        0,
                        Span::styled(
                            crate::ui::text_width::glyphs::select("  │ ", "  | "),
                            Style::default().fg(guide_color).underline_color(marker),
                        ),
                    );
                }
                out.extend(output);
            } else {
                let style = Style::default().fg(if block.kind == BlockKind::Code {
                    theme::CODE_LABEL
                } else if block.kind == BlockKind::Event {
                    theme::MUTED
                } else {
                    theme::EMPHASIS
                });
                out.extend(
                    text.lines()
                        .map(|line| Line::from(Span::styled(line.to_owned(), style))),
                );
            }
        }
    }
    out.role = LineRole::Meta;
    for notice in omission_notices(language, node) {
        out.push(Line::from(Span::styled(
            notice,
            Style::default().fg(theme::MUTED),
        )));
    }
    out
}
fn omission_notices(language: UiLanguage, node: &UiNode) -> Vec<String> {
    let mut parts = Vec::new();
    let end = node
        .base_char_offset
        .saturating_add(node.text.chars().count())
        .min(node.total_chars);
    if node.base_char_offset > 0 || end < node.total_chars {
        parts.push(
            Message::SourceWindow {
                start: node.base_char_offset.saturating_add(1),
                end,
                total: node.total_chars,
                live: node.presentation.provisional_stream,
            }
            .render(language),
        );
    }
    if node.presentation.body_omitted_chars > 0 {
        parts.push(
            Message::BodyOmitted {
                count: node.presentation.body_omitted_chars,
            }
            .render(language),
        );
    }
    if node.presentation.body_extent_unknown {
        parts.push(
            if node.presentation.provisional_stream {
                Text::StreamGap
            } else {
                Text::UnknownExtent
            }
            .render(language)
            .into(),
        );
    }
    if node.presentation.ui_messages.limited {
        parts.push(Text::MetadataLimited.render(language).into());
    }
    if parts.is_empty() {
        parts
    } else {
        vec![parts.join(crate::ui::text_width::glyphs::separator())]
    }
}
fn tool_header_lines(
    language: UiLanguage,
    tool: &crate::presentation::ToolPresentation,
    request: bool,
    width: u16,
    links: &[crate::presentation::PresentationLink],
    targets: &mut Vec<std::sync::Arc<str>>,
) -> Vec<Line<'static>> {
    let mut header = vec![Line::from(Span::styled(
        crate::presentation::messages::tool_heading(tool, request, language),
        Style::default().fg(if tool.requires_attention() {
            theme::ATTENTION
        } else {
            theme::TOOL
        }),
    ))];
    annotate_links(&mut header, links, targets);
    hanging_wrap(header, width)
}
fn link_target(targets: &mut Vec<std::sync::Arc<str>>, url: &str) -> usize {
    targets
        .iter()
        .position(|target| target.as_ref() == url)
        .unwrap_or_else(|| {
            targets.push(std::sync::Arc::from(url));
            targets.len() - 1
        })
}
fn annotate_links(
    lines: &mut [Line<'static>],
    links: &[crate::presentation::PresentationLink],
    targets: &mut Vec<std::sync::Arc<str>>,
) {
    for line in lines {
        let mut spans = Vec::new();
        for span in std::mem::take(&mut line.spans) {
            // Semantic Markdown bindings (including unavailable targets) and previously
            // annotated tool spans already have an identity. Never infer from their label.
            if span.style.underline_color.is_some() {
                spans.push(span);
                continue;
            }
            let text = span.content.as_ref();
            let mut matches = links
                .iter()
                .flat_map(|link| {
                    text.match_indices(&link.url)
                        .map(|(start, _)| (start, start + link.url.len(), link.url.as_str()))
                })
                .collect::<Vec<_>>();
            matches.sort_by_key(|(start, end, _)| (*start, std::cmp::Reverse(*end)));
            let mut end = 0;
            for (start, until, url) in matches {
                if start < end {
                    continue;
                }
                if start > end {
                    spans.push(Span::styled(text[end..start].to_owned(), span.style));
                }
                let index = link_target(targets, url);
                spans.push(Span::styled(
                    text[start..until].to_owned(),
                    span.style
                        .fg(theme::LINK)
                        .add_modifier(Modifier::UNDERLINED)
                        .underline_color(crate::terminal_links::marker_color(index)),
                ));
                end = until;
            }
            if end < text.len() {
                spans.push(Span::styled(text[end..].to_owned(), span.style));
            }
        }
        line.spans = spans;
    }
}
pub(super) fn hanging_wrap(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    use crate::ui::text_width::UnicodeWidthStr;
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        widgets::{Paragraph, Widget, Wrap},
    };
    let mut out = Vec::new();
    for mut line in lines {
        crate::presentation::expand_tabs(&mut line);
        if line.width() <= usize::from(width) {
            out.push(line);
            continue;
        }
        let paragraph = Paragraph::new(vec![line]).wrap(Wrap { trim: false });
        let height = paragraph.line_count(width).min(u16::MAX as usize) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        paragraph.render(area, &mut buffer);
        for y in 0..height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut x = 0;
            while x < width {
                let cell = &buffer[(x, y)];
                let symbol = cell.symbol();
                if let Some(last) = spans.last_mut().filter(|last| last.style == cell.style()) {
                    last.content.to_mut().push_str(symbol);
                } else {
                    spans.push(Span::styled(symbol.to_owned(), cell.style()));
                }
                x += symbol.width().max(1) as u16;
            }
            if let Some(last) = spans.last_mut() {
                last.content = last.content.trim_end().to_owned().into();
            }
            out.push(Line::from(spans));
        }
    }
    out
}

pub(super) fn indented_wrap(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    // Preserve the viewport at widths too narrow to fit both the indent and text.
    let indent = if width >= 3 { 2 } else { 0 };
    hanging_wrap(lines, width.saturating_sub(indent).max(1))
        .into_iter()
        .map(|mut line| {
            if line.width() > 0 && indent > 0 {
                let style = line
                    .spans
                    .first()
                    .map_or(line.style, |span| line.style.patch(span.style));
                line.spans.insert(0, Span::styled("  ", style));
            }
            line
        })
        .collect()
}

fn node_lines(
    language: UiLanguage,
    node: &UiNode,
    expanded: bool,
    start: usize,
    width: u16,
    links: &mut Vec<std::sync::Arc<str>>,
) -> Vec<Line<'static>> {
    node_lines_with_header(language, node, expanded, start, width, links, true)
}
fn node_lines_with_header(
    language: UiLanguage,
    node: &UiNode,
    expanded: bool,
    start: usize,
    width: u16,
    links: &mut Vec<Arc<str>>,
    show_header: bool,
) -> Vec<Line<'static>> {
    // A saved context line (#183) is model input only; it renders no rows.
    if node.role == "context" {
        return Vec::new();
    }
    let conversational = conversational(node);
    let tool_only = !node.presentation.blocks.is_empty()
        && node
            .presentation
            .blocks
            .iter()
            .all(|block| block.kind == BlockKind::ToolCall);
    let mut source = node_lines_inner(
        language,
        node,
        expanded,
        start,
        width.saturating_sub(2).max(1),
        links,
        show_header,
    );
    // Preserve the legacy empty conversational placeholder: removing it changes
    // page boundary row counts when an uncached/unknown node is reprojected.
    if source.rows.is_empty() && conversational && !tool_only {
        source.role = LineRole::Body;
        source.push(Line::default());
    }
    let body_hint = node.presentation.body_evicted
        || (node.text.is_empty() && node.total_chars > 0)
        || (node.presentation.blocks.is_empty()
            && matches!(node.role.as_str(), "developer" | "system" | "tool"));
    let mut lines = Vec::new();
    let mut body_marked = false;
    for (role, mut line) in source.rows {
        annotate_links(
            std::slice::from_mut(&mut line),
            &node.presentation.links,
            links,
        );
        let inline_summary = !conversational
            && !tool_only
            && !body_hint
            && lines.len() == 1
            && matches!(
                node.presentation.kind,
                PresentationKind::Spill | PresentationKind::Unknown
            );
        let indent_body = !conversational
            && !tool_only
            && node.presentation.kind != PresentationKind::Event
            && matches!(role, LineRole::Body | LineRole::Reasoning)
            && !inline_summary;
        let rows = if conversational && !tool_only {
            hanging_wrap(vec![line], width.saturating_sub(2).max(1))
        } else if indent_body {
            indented_wrap(vec![line], width)
        } else {
            vec![line]
        };
        for mut row in rows {
            if conversational && !tool_only {
                let marker = if matches!(role, LineRole::Body) && !body_marked {
                    body_marked = true;
                    Span::styled(
                        if node.role == "user" {
                            input_marker(node)
                        } else {
                            format!("{} ", RowKind::Assistant.mark())
                        },
                        Style::default().fg(if node.role == "user" {
                            RowKind::Human.spec().2
                        } else {
                            RowKind::Assistant.spec().2
                        }),
                    )
                } else {
                    if row.spans.is_empty() {
                        lines.push(row);
                        continue;
                    }
                    Span::raw("  ")
                };
                let mut marker = marker;
                if node.role == "user" {
                    marker.style.underline_color = Some(INPUT_DECORATION_MARKER);
                }
                row.spans.insert(0, marker);
            }
            lines.push(row);
        }
    }
    if !conversational
        && !tool_only
        && !body_hint
        && matches!(
            node.presentation.kind,
            PresentationKind::Spill | PresentationKind::Unknown
        )
        && lines.len() > 1
        && !lines[1].spans.is_empty()
    {
        let first_body = lines.remove(1);
        lines[0].spans.push(Span::raw("  "));
        lines[0].spans.extend(first_body.spans);
    }
    if node.presentation.kind == PresentationKind::Event && lines.len() > 1 {
        let body = lines.split_off(1);
        lines.extend(indented_wrap(body, width));
    }
    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    lines
}
fn notice_message(message: &Message) -> Option<&Message> {
    match message {
        Message::EventSummary { title, .. } => notice_message(title),
        Message::EventIdentity { .. } | Message::EventTitle { .. } => Some(message),
        _ => None,
    }
}

fn notice_kind(node: &UiNode) -> Option<&str> {
    match node
        .presentation
        .ui_messages
        .title
        .as_ref()
        .and_then(notice_message)
    {
        Some(Message::EventIdentity { kind, .. } | Message::EventTitle { kind }) => Some(kind),
        _ => node
            .presentation
            .fields
            .iter()
            .find(|f| f.key == "/kind" && !f.value_omitted)
            .map(|f| f.value.as_str()),
    }
}

fn notice_symbol(kind: Option<&str>) -> &'static str {
    use crate::ui::text_width::glyphs;
    match kind.unwrap_or("") {
        "shell_ended" => glyphs::SHELL,
        "child_turn_completed" | "child_run_ended" | "child_report" => glyphs::CHILD,
        "recovery_wait" | "child_recovery_wait" | "reply_reminder" | "reply_outcome" => {
            glyphs::WAIT
        }
        "thread_settings"
        | "connection_changed"
        | "model_changed"
        | "settings_changed"
        | "instructions_observed"
        | "instruction_changed"
        | "agents_changed"
        | "agents_reference" => glyphs::SETTINGS,
        "previous_instance_interrupted"
        | "app_shutdown"
        | "recovery_resumed"
        | "interrupted_messages_recovered" => glyphs::LIFECYCLE,
        "index_state" => glyphs::INDEX,
        "turn_started" | "run_started" => glyphs::START,
        _ => RowKind::Unknown.mark(),
    }
}

enum BackgroundLabel {
    Requested(bool),
    Running(crate::presentation::ToolReasonKind),
}

fn background_label(language: UiLanguage, background: BackgroundLabel) -> &'static str {
    use crate::presentation::ToolReasonKind;
    match background {
        BackgroundLabel::Requested(true) => {
            crate::ui::i18n::local!(language, "背景指定あり", "Background requested")
        }
        BackgroundLabel::Requested(false) => {
            crate::ui::i18n::local!(language, "背景指定なし", "Foreground requested")
        }
        BackgroundLabel::Running(ToolReasonKind::HumanBackgrounded) => {
            Text::HumanBackground.render(language)
        }
        BackgroundLabel::Running(ToolReasonKind::AdditionalInput) => {
            Text::InputBackground.render(language)
        }
        BackgroundLabel::Running(_) => Text::Background.render(language),
    }
}

fn exit_code_label(code: i64) -> String {
    format!("exit {code}")
}

fn notice_row(view: &ThreadView, node: &UiNode, width: u16, prefix: String) -> Line<'static> {
    use crate::ui::text_width::glyphs;
    let language = view.language;
    let field = |key: &str| {
        node.presentation
            .fields
            .iter()
            .find(|f| f.key == key && !f.value_omitted)
            .map(|f| f.value.as_str())
    };
    let kind = notice_kind(node).unwrap_or("");
    let saved = saved_failure_text(node, language);
    let title = if let Some((_, reason)) = &saved {
        let count = field("/saved_model_failures/count").unwrap_or("?");
        let summary = format!(
            "{}{count}{}",
            crate::ui::i18n::local!(language, "記録したモデル失敗", ""),
            crate::ui::i18n::local!(language, "件", " model failures recorded")
        );
        format!("{summary}{}{}", glyphs::separator(), reason)
    } else if let Some(Message::IndexPause {
        at: Some(at),
        reason: Some(reason),
    }) = node
        .presentation
        .ui_messages
        .title
        .as_ref()
        .map(|m| match m {
            Message::EventSummary { title, .. } => title.as_ref(),
            _ => m,
        })
    {
        format!(
            "{}{}{at}{}{reason}",
            glyphs::select("/index → T", "/index > T"),
            glyphs::separator(),
            glyphs::separator()
        )
    } else {
        match &node.presentation.ui_messages.notice {
            Some(Message::CommandNotice { command, .. }) => view
                .describe_calls
                .then(|| field("/ui_notice/description"))
                .flatten()
                .filter(|_| {
                    !(view.items.active
                        && view.items.focus.as_ref() == Some(&ItemKey::Node(node.node_id.clone())))
                })
                .or(command.as_deref())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(Text::NoticeCommandUnrecorded.render(language))
                .to_owned(),
            Some(Message::ThreadNotice { title }) => title
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(Text::NoticeUntitled.render(language))
                .to_owned(),
            Some(Message::Text(text)) => text.render(language).to_owned(),
            _ => node
                .presentation
                .ui_messages
                .title
                .as_ref()
                .map(|message| message.render(language))
                .unwrap_or_else(|| {
                    if language == UiLanguage::En {
                        Text::ReviewChange.render(language).to_owned()
                    } else {
                        node.presentation.title.clone()
                    }
                }),
        }
    };
    let title = crate::presentation::sanitize_plain(&title, 0)
        .0
        .replace(['\n', '\r'], " ");
    let (status, code, elapsed) = match node
        .presentation
        .ui_messages
        .title
        .as_ref()
        .and_then(notice_message)
    {
        Some(Message::EventIdentity {
            status,
            code,
            elapsed_ms,
            ..
        }) => (status.as_deref(), *code, *elapsed_ms),
        _ => (None, None, None),
    };
    let mut outcome = match (status, code) {
        (_, Some(code)) if kind == "shell_ended" => exit_code_label(code),
        (Some(status), code) => crate::presentation::messages::tool_state(status, code, language),
        (None, Some(code)) => Message::Exit { code }.render(language),
        _ => String::new(),
    };
    if kind == "child_turn_completed" && matches!(status, None | Some("completed")) {
        outcome = crate::ui::i18n::local!(language, "回答", "Answer").into();
    } else if kind == "child_report" && matches!(status, None | Some("completed")) {
        outcome = crate::ui::i18n::local!(language, "報告", "Report").into();
    } else if kind == "child_run_ended" && status == Some("completed") {
        outcome = crate::ui::i18n::local!(language, "完了", "Completed").into();
    }
    let signal = node
        .presentation
        .fields
        .iter()
        .find(|f| f.key.ends_with("/exit_signal") && !f.value_omitted)
        .and_then(|f| f.value.parse::<i64>().ok());
    if let Some(signal) = signal {
        let name = match signal {
            1 => "SIGHUP".into(),
            2 => "SIGINT".into(),
            3 => "SIGQUIT".into(),
            9 => "SIGKILL".into(),
            15 => "SIGTERM".into(),
            _ => format!(
                "{}{signal}",
                crate::ui::i18n::local!(language, "シグナル ", "signal ")
            ),
        };
        if outcome.is_empty() {
            outcome = crate::ui::i18n::local!(language, "停止", "Stopped").into();
        }
        outcome.push_str(&format!("({name})"));
    }
    let elapsed = elapsed.map(|ms| Message::Elapsed { ms, decimal: false }.render(language));
    let background = match &node.presentation.ui_messages.notice {
        Some(Message::CommandNotice {
            background: Some(requested),
            ..
        }) => Some(background_label(language, BackgroundLabel::Requested(*requested)).to_owned()),
        _ => None,
    };
    let metadata = [
        (!outcome.is_empty()).then_some(outcome),
        elapsed,
        background,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(glyphs::separator());
    let (label, gap, right) =
        super::live_work::row_columns(usize::from(width), &prefix, &title, &metadata, "");
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(theme::TEXT)),
        Span::raw(label),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(theme::MUTED)),
    ])
}

fn is_notice(node: &UiNode) -> bool {
    node.presentation.kind == PresentationKind::Event && node.role != "received_response"
}

fn adjacent_tool_pair(call: &UiNode, result: &UiNode) -> bool {
    let calls = &call.presentation.blocks;
    (!result.text.is_empty() || result.total_chars == 0)
        && result
            .presentation
            .blocks
            .first()
            .is_some_and(|block| block.kind == BlockKind::ToolResult && block.tool.is_some())
        && !calls.is_empty()
        && calls.iter().all(|block| {
            block.kind == BlockKind::ToolCall
                && block
                    .tool
                    .as_ref()
                    .and_then(|tool| tool.execution_id.as_deref())
                    .is_some_and(|execution| {
                        result.presentation.blocks.iter().any(|other| {
                            other.kind == BlockKind::ToolResult
                                && other
                                    .tool
                                    .as_ref()
                                    .and_then(|tool| tool.execution_id.as_deref())
                                    == Some(execution)
                        })
                    })
        })
}
fn interruption_group(nodes: &[UiNode]) -> Option<usize> {
    let details = nodes
        .iter()
        .take_while(|node| {
            node.presentation.kind == PresentationKind::Interrupted
                && node
                    .presentation
                    .fields
                    .iter()
                    .any(|field| field.key == "/section")
        })
        .count();
    if details == 0 {
        return None;
    }
    let summary = nodes.get(details)?;
    if summary.presentation.kind != PresentationKind::Interrupted
        || !summary.presentation.fields.iter().any(|field| {
            field.key == "/kind"
                && matches!(
                    field.value.as_str(),
                    "previous_instance_interrupted" | "app_shutdown"
                )
        })
    {
        return None;
    }
    let references = summary
        .presentation
        .fields
        .iter()
        .filter_map(|field| {
            if field.value_omitted {
                return None;
            }
            let path = field.key.strip_prefix("/change/")?;
            let (section, rest) = path.split_once('/')?;
            if !matches!(
                section,
                "runs"
                    | "turns"
                    | "shells"
                    | "owned_children"
                    | "unreflected_messages"
                    | "cancelled_waits"
                    | "pending_operations_with_unknown_targets"
            ) {
                return None;
            }
            rest.strip_prefix("detail_node_ids/")?
                .parse::<usize>()
                .ok()?;
            Some((section, field.value.as_str()))
        })
        .collect::<Vec<_>>();
    nodes[..details]
        .iter()
        .all(|node| {
            node.presentation
                .fields
                .iter()
                .find(|field| field.key == "/section")
                .is_some_and(|section| {
                    references.contains(&(section.value.as_str(), node.node_id.as_str()))
                })
        })
        .then_some(details)
}
/// One physical wrapped row and its nearest original Unicode position. Decoration
/// rows intentionally share an anchor; the viewport keeps their row displacement.
#[derive(Clone, Debug)]
pub(super) struct RowSource {
    pub node: usize,
    /// Exclusive end of the source nodes represented by this physical row.
    /// Event groups retain their first-node anchor while covering every member.
    pub covered_end: usize,
    pub offset: usize,
    pub item: Option<Arc<ItemRegion>>,
}
#[cfg(test)]
pub(super) fn history(
    view: &ThreadView,
    width: u16,
) -> (Vec<Line<'static>>, Vec<std::sync::Arc<str>>, Vec<RowSource>) {
    history_with_cache(view, width, None, 0)
}
/// Canonical source window examined by the projection, including zero-row
/// quiet/duplicate members. Navigation must not infer this from visible rows.
pub(super) fn projection_bounds(view: &ThreadView) -> (usize, usize) {
    // Layout only a contiguous neighborhood, independent of retained cache size.
    // A single body is already bounded by the frontend's source-window limit.
    let focus = super::projection_node_index(view);
    let mut begin = focus.min(view.nodes().len());
    let mut end = (begin + 1).min(view.nodes().len());
    let mut bytes = view.nodes().get(begin).map_or(0, |n| n.text.len());
    loop {
        let mut added = false;
        for before in [true, false] {
            let next = if before {
                begin.checked_sub(1)
            } else {
                (end < view.nodes().len()).then_some(end)
            };
            if let Some(next) = next {
                let cost = view.nodes()[next].text.len();
                if end - begin < 128 && bytes.saturating_add(cost) <= 192 * 1024 {
                    bytes += cost;
                    if before {
                        begin = next;
                    } else {
                        end += 1;
                    }
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    (begin, end)
}
pub(super) fn history_with_cache(
    view: &ThreadView,
    width: u16,
    mut cache: Option<&mut super::HistoryLayoutCache>,
    budget: usize,
) -> (Vec<Line<'static>>, Vec<std::sync::Arc<str>>, Vec<RowSource>) {
    let language = view.language;
    let mut links = Vec::new();
    let mut lines = Vec::new();
    let mut sources = Vec::new();
    let (begin, end) = projection_bounds(view);
    if view.nodes()[begin..end].iter().any(|node| {
        node.presentation.kind == PresentationKind::Event
            || node
                .presentation
                .blocks
                .iter()
                .any(|block| block.tool.is_some())
    }) {
        return typed_history(view, begin, end, width, cache, budget);
    }
    let mut index = begin;
    while index < end {
        let unit_end = if let Some(summary) = interruption_group(&view.nodes()[index..end]) {
            index + summary + 1
        } else if view
            .nodes()
            .get(index + 1)
            .filter(|_| index + 1 < end)
            .is_some_and(|result| adjacent_tool_pair(&view.nodes()[index], result))
        {
            index + 2
        } else {
            index + 1
        };
        let unit_key = cache
            .as_ref()
            .and_then(|_| super::RenderUnitKey::new(view, index, unit_end, width, &links));
        if let Some(hit) = unit_key
            .as_ref()
            .and_then(|key| cache.as_mut().and_then(|cache| cache.unit(key)))
        {
            lines.extend(hit.styled.iter().cloned());
            sources.extend_from_slice(&hit.sources);
            links = hit.targets.clone();
            index = unit_end;
            continue;
        }
        let row_start = lines.len();

        let begin = index;
        let mut rendered_index = index;
        let mut group_header_rows = 0;
        let node = &view.nodes()[index];
        let chunk = if let Some(summary) = interruption_group(&view.nodes()[index..end]) {
            index += summary + 1;
            rendered_index = index - 1;
            let summary = &view.nodes()[rendered_index];
            let mut chunk = node_lines(
                language,
                summary,
                false,
                summary.base_char_offset,
                width,
                &mut links,
            );
            group_header_rows = chunk.first().map_or(0, |line| {
                hanging_wrap(vec![line.clone()], width.max(1)).len()
            });
            chunk.push(Line::from(Span::styled(
                Message::SourceCount {
                    count: index - begin,
                }
                .render(language),
                Style::default().fg(theme::MUTED),
            )));
            chunk
        } else if view
            .nodes()
            .get(index + 1)
            .filter(|_| index + 1 < end)
            .is_some_and(|result| adjacent_tool_pair(node, result))
        {
            index += 2;
            rendered_index = index - 1;
            let result = &view.nodes()[rendered_index];
            // Reuse precisely the renderer's header wrapping. It is already
            // physical rows before the final group wrap, including long commands.
            if let Some(tool) = result
                .presentation
                .blocks
                .first()
                .and_then(|block| block.tool.as_ref())
            {
                let header = tool_header_lines(
                    language,
                    tool,
                    false,
                    width.saturating_sub(2).max(1),
                    &result.presentation.links,
                    &mut links,
                );
                group_header_rows = hanging_wrap(header, width.max(1)).len();
            }
            let mut chunk = node_lines(
                language,
                result,
                false,
                result.base_char_offset,
                width,
                &mut links,
            );
            // Result commands are enriched from their exact call source. Retain
            // its explicit URL targets when rendering that shared heading.
            annotate_links(&mut chunk, &node.presentation.links, &mut links);
            chunk
        } else {
            index += 1;
            node_lines(
                language,
                node,
                view.reasoning_expanded.contains(&node.node_id),
                node.base_char_offset,
                width,
                &mut links,
            )
        };
        if chunk.is_empty() {
            continue;
        }
        let node = &view.nodes()[rendered_index];
        let chunk = if view
            .items
            .is_open(&ItemKey::Node(node.node_id.clone()), true, "")
        {
            chunk
        } else {
            vec![Line::from(Text::Collapsed.render(language))]
        };
        let mut chunk = hanging_wrap(chunk, width.max(1));
        user_band(language, &mut chunk, node, width, view.palette);
        let source_starts = node
            .presentation
            .blocks
            .iter()
            .filter(|b| {
                view.reasoning_expanded.contains(&node.node_id) || b.kind != BlockKind::Reasoning
            })
            .map(|b| b.source_start.saturating_sub(node.base_char_offset))
            .collect::<std::collections::BTreeSet<_>>();
        let block_bytes = node
            .text
            .char_indices()
            .enumerate()
            .filter_map(|(character, (byte, _))| source_starts.contains(&character).then_some(byte))
            .collect::<Vec<_>>();
        let mut byte: usize = 0;
        let mut offset = node.base_char_offset;
        let mut consumed = 0;
        // Extra work for known excerpt gaps is limited to one forward source
        // window in total, independently of the number of decoration rows.
        let mut tail_search_budget = node.text.len();
        let mut excerpt_jump = false;
        for (row, line) in chunk.into_iter().enumerate() {
            if row < group_header_rows && rendered_index != begin {
                sources.push(RowSource {
                    item: None,
                    node: if row == 0 { begin } else { rendered_index },
                    covered_end: index,
                    offset: if row == 0 {
                        view.nodes()[begin].base_char_offset
                    } else {
                        node.base_char_offset
                    },
                });
                lines.push(line);
                continue;
            }
            let text = body_spans(&line)
                .map(|s| s.content.as_ref())
                .collect::<String>();
            let text = source_candidate(&text);
            let gap_row = line
                .spans
                .iter()
                .any(|span| span.style.underline_color == Some(EXCERPT_GAP_MARKER));
            if gap_row {
                excerpt_jump = true;
                // A generated omission row has no literal source of its own.
                // Keep the previous real anchor even if its label also occurs
                // inside the omitted user output; do not spend search budget.
                sources.push(RowSource {
                    item: None,
                    node: rendered_index,
                    covered_end: index,
                    offset,
                });
                lines.push(line);
                continue;
            }
            let output_body = !gap_row
                && line
                    .spans
                    .iter()
                    .any(|span| span.style.underline_color == Some(OUTPUT_ROW_MARKER));
            // Exact visible substrings give original character positions for
            // plain text, code and Markdown prose. Generated decoration maps to
            // the preceding source position, never a fabricated character ratio.
            let search_end = node.text.floor_char_boundary(
                byte.saturating_add(4096.max(text.len()))
                    .min(node.text.len()),
            );
            let whole = (!text.is_empty())
                .then(|| node.text[byte..search_end].find(text))
                .flatten()
                .map(|found| (found, text.len()));
            // Markdown markers interrupt whole-line matching. OSS spans retain
            // the natural-language pieces, so use their first real text span
            // rather than sending formatted prose back to the block's start.
            let matched = whole.or_else(|| {
                body_spans(&line).find_map(|span| {
                    let part = source_candidate(&span.content);
                    (!part.is_empty())
                        .then(|| node.text[byte..search_end].find(part))
                        .flatten()
                        .map(|found| (found, part.len()))
                })
            });
            let matched = matched
                .map(|(found, length)| (byte + found, length))
                .or_else(|| {
                    block_bytes
                        .iter()
                        .copied()
                        .filter(|start| *start > byte)
                        .find_map(|start| {
                            let end = node.text.floor_char_boundary(
                                (start + 4096.max(text.len())).min(node.text.len()),
                            );
                            let source = &node.text[start..end];
                            (!text.is_empty())
                                .then(|| source.find(text))
                                .flatten()
                                .map(|found| (start + found, text.len()))
                                .or_else(|| {
                                    body_spans(&line).find_map(|span| {
                                        let part = source_candidate(&span.content);
                                        (!part.is_empty())
                                            .then(|| source.find(part))
                                            .flatten()
                                            .map(|found| (start + found, part.len()))
                                    })
                                })
                        })
                });
            // Only the real output following our typed excerpt marker may
            // cross the fast lookup bound. Headers/notices cannot spend this.
            let matched = matched.or_else(|| {
                if excerpt_jump && output_body {
                    excerpt_jump = false;
                    excerpt_tail_match(&node.text, text, byte, &mut tail_search_budget)
                } else {
                    None
                }
            });
            if let Some((start, length)) = matched {
                consumed += node.text[byte..start].chars().count();
                offset = node.base_char_offset + consumed;
                let mut end = start + length;
                if &node.text[start..end] != text {
                    let mut after_first = false;
                    for span in body_spans(&line) {
                        let part = source_candidate(&span.content);
                        if part.is_empty() {
                            continue;
                        }
                        if !after_first {
                            after_first = part == &node.text[start..start + length];
                            continue;
                        }
                        let limit = node.text.floor_char_boundary(
                            end.saturating_add(4096.max(part.len()))
                                .min(node.text.len()),
                        );
                        if let Some(found) = node.text[end..limit].find(part) {
                            end += found + part.len();
                        }
                    }
                }
                consumed += node.text[start..end].chars().count();
                byte = end;
            }
            sources.push(RowSource {
                item: None,
                node: rendered_index,
                covered_end: index,
                offset,
            });
            lines.push(line);
        }
        sources.push(RowSource {
            item: None,
            node: rendered_index,
            covered_end: index,
            offset,
        });
        lines.push(Line::default());
        let region = Arc::new(node_region(view, node));
        for source in &mut sources[row_start..] {
            source.item = Some(region.clone());
        }
        if let (Some(key), Some(cache)) = (unit_key, cache.as_mut()) {
            cache.insert_unit(
                key,
                std::sync::Arc::new(super::HistoryLayout {
                    text: String::new(),
                    styled: lines[row_start..].to_vec(),
                    lines: lines.len() - row_start,
                    targets: links.clone(),
                    sources: sources[row_start..].to_vec(),
                }),
                budget,
            );
        }
    }
    (lines, links, sources)
}
fn input_kind(node: &UiNode) -> RowKind {
    use crate::presentation::InputSource;
    match node.presentation.input_source {
        InputSource::Human => RowKind::Human,
        InputSource::Unknown => RowKind::External,
        InputSource::Parent => RowKind::Parent,
        InputSource::SelfModel => RowKind::SelfModel,
        InputSource::OtherModel => RowKind::OtherModel,
        InputSource::Revisit => RowKind::Revisit,
    }
}
fn input_marker(node: &UiNode) -> String {
    format!("{} ", input_kind(node).mark())
}

struct InputBandRow {
    mark: Option<Span<'static>>,
    body: Line<'static>,
    metadata: Option<Span<'static>>,
}
impl InputBandRow {
    fn into_line(mut self, width: u16, style: Style) -> Line<'static> {
        let decoration = |text| Span::styled(text, style.underline_color(INPUT_DECORATION_MARKER));
        self.body.style = self.body.style.patch(Style { fg: None, ..style });
        self.body.style.bg = style.bg;
        for span in &mut self.body.spans {
            span.style.bg = style.bg;
        }
        if let Some(mut mark) = self.mark {
            mark.style.bg = style.bg;
            self.body.spans.insert(0, mark);
        }
        let used = self.body.width();
        if let Some(mut metadata) = self
            .metadata
            .filter(|value| used + value.width() + 2 <= usize::from(width))
        {
            self.body.spans.push(decoration(
                " ".repeat(usize::from(width) - used - metadata.width()),
            ));
            metadata.style.bg = style.bg;
            self.body.spans.push(metadata);
        } else if used < usize::from(width) {
            self.body
                .spans
                .push(decoration(" ".repeat(usize::from(width) - used)));
        }
        self.body
    }
}
fn input_metadata(
    language: UiLanguage,
    node: &UiNode,
    palette: theme::Palette,
) -> Option<Span<'static>> {
    use crate::presentation::InputSource;
    let mut parts = Vec::new();
    if !matches!(
        node.presentation.input_source,
        InputSource::Human | InputSource::Unknown
    ) {
        parts.push(row_label(input_kind(node), language).to_owned());
    }
    if let Some(label) = node.presentation.input_sender_label.as_deref() {
        let label = crate::presentation::sanitize_plain(label, 0).0;
        let label: String = label.chars().take(64).collect();
        if !label.trim().is_empty() {
            parts.push(label.replace(['\n', '\r'], " "));
        }
    }
    if let Some(time) = node
        .presentation
        .created_at
        .as_deref()
        .and_then(|saved| saved.get(11..16))
        .filter(|value| value.is_ascii() && !value.is_empty())
    {
        parts.push(time.to_owned());
    }
    (!parts.is_empty()).then(|| {
        Span::styled(
            parts.join(crate::ui::text_width::glyphs::separator()),
            Style::default()
                .fg(palette.muted())
                .underline_color(INPUT_DECORATION_MARKER),
        )
    })
}
fn user_band(
    language: UiLanguage,
    lines: &mut [Line<'static>],
    node: &UiNode,
    width: u16,
    palette: theme::Palette,
) {
    if node.role != "user" {
        return;
    }
    let mut metadata = input_metadata(language, node, palette);
    for line in lines {
        let mut body = std::mem::take(line);
        let mark = body
            .spans
            .first()
            .is_some_and(|span| span.style.underline_color == Some(INPUT_DECORATION_MARKER))
            .then(|| body.spans.remove(0));
        *line = InputBandRow {
            mark,
            body,
            metadata: metadata.take(),
        }
        .into_line(width, palette.user_band(node.presentation.input_source));
    }
}
fn node_region(view: &ThreadView, node: &UiNode) -> ItemRegion {
    let execution = if node.presentation.kind == PresentationKind::Event {
        node.presentation
            .fields
            .iter()
            .find(|f| f.key == "/payload/execution_id")
            .map(|f| f.value.clone())
    } else {
        None
    };
    let mut source_node_ids = vec![node.node_id.clone()];
    if let Some(execution) = &execution {
        for tool in view
            .nodes()
            .iter()
            .flat_map(|n| n.presentation.blocks.iter())
            .filter_map(|b| b.tool.as_ref())
            .filter(|t| t.execution_id.as_ref() == Some(execution))
            .take(128)
        {
            for id in [&tool.call_node_id, &tool.result_node_id]
                .into_iter()
                .flatten()
            {
                if source_node_ids.len() < 8 && !source_node_ids.contains(id) {
                    source_node_ids.push(id.clone());
                }
            }
        }
    }
    ItemRegion {
        key: ItemKey::Node(node.node_id.clone()),
        source_node_ids,
        primary_node_id: node.node_id.clone(),
        execution_id: None,
        expandable: true,
        default_open: true,
        outcome: String::new(),
    }
}
pub(super) fn tool_outcome(tool: &crate::presentation::ToolPresentation) -> String {
    let current = tool
        .current_state
        .as_deref()
        .filter(|state| *state != "observation_unavailable");
    let state = current.unwrap_or(&tool.status);
    let code = if current.is_some() {
        tool.current_exit_code.or(tool.exit_code)
    } else {
        tool.exit_code
    };
    let kind = if state == "exited" && code.is_none() {
        RowKind::Unknown
    } else {
        crate::presentation::rows::tool_kind(state, tool.requires_attention())
    };
    // Progress and metadata refinements must not undo an explicit close.
    let disclosure = format!("{kind:?}");
    match tool.output_revision {
        Some((sequence, complete)) => format!("{disclosure}|output:{sequence}:{complete}"),
        None => disclosure,
    }
}

fn tool_metadata(
    language: UiLanguage,
    tool: &crate::presentation::ToolPresentation,
    elapsed: Option<u64>,
) -> (RowKind, String, Option<String>) {
    let status = tool.current_state.as_deref().unwrap_or(&tool.status);
    let code = if tool
        .current_state
        .as_deref()
        .is_some_and(|s| s != "observation_unavailable")
    {
        tool.current_exit_code
    } else {
        tool.exit_code
    };
    let kind = if status == "exited" && code.is_none() {
        RowKind::Unknown
    } else {
        crate::presentation::rows::tool_kind(status, tool.requires_attention())
    };
    let background = tool.background
        || status == "background_current"
        || matches!(
            tool.reason_kind,
            crate::presentation::ToolReasonKind::HumanBackgrounded
                | crate::presentation::ToolReasonKind::AdditionalInput
        );
    let mut parts = Vec::new();
    if background {
        parts.push(
            background_label(language, BackgroundLabel::Running(tool.reason_kind)).to_owned(),
        );
    }
    let outcome = if kind == RowKind::Unknown {
        Some(row_label(kind, language).to_owned())
    } else {
        code.map(exit_code_label).or_else(|| {
            matches!(kind, RowKind::Failure | RowKind::Cancelled)
                .then(|| row_label(kind, language).to_owned())
        })
    };
    let critical = outcome
        .clone()
        .filter(|_| matches!(kind, RowKind::Failure | RowKind::Cancelled));
    if let Some(outcome) = outcome {
        parts.push(outcome);
    }
    if let Some(seconds) = elapsed {
        parts.push(
            Message::Elapsed {
                ms: seconds.saturating_mul(1000),
                decimal: false,
            }
            .render(language),
        );
    } else if let Some(ms) = tool.elapsed_ms {
        parts.push(Message::Elapsed { ms, decimal: true }.render(language));
    } else if kind == RowKind::Running && tool.started_at.is_some() {
        parts.push(Text::ElapsedUnavailable.render(language).into());
    }
    (
        kind,
        parts.join(crate::ui::text_width::glyphs::separator()),
        critical,
    )
}
fn tool_text_fit(text: &str, width: usize) -> String {
    use crate::ui::text_width::UnicodeWidthStr;
    use unicode_segmentation::UnicodeSegmentation;
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 1;
    for glyph in text.graphemes(true) {
        if used + glyph.width() > width {
            break;
        }
        result.push_str(glyph);
        used += glyph.width();
    }
    result.push_str(crate::ui::text_width::glyphs::ellipsis());
    result
}
fn tool_row(
    mark: Span<'static>,
    mut body: Line<'static>,
    metadata: String,
    critical: Option<String>,
    width: u16,
    color: Color,
) -> Line<'static> {
    use crate::ui::text_width::UnicodeWidthStr;
    let width_usize = usize::from(width);
    let metadata = if mark.width() + body.width() + metadata.width() + 2 <= width_usize {
        Some(metadata)
    } else if let Some(critical) = critical {
        // Noncritical metadata never takes body space. Attention is the exception.
        let available = width_usize.saturating_sub(mark.width() + critical.width() + 2);
        let text = body
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let style = body
            .spans
            .first()
            .map_or(Style::default(), |span| span.style);
        body = Line::from(Span::styled(tool_text_fit(&text, available), style));
        Some(critical)
    } else {
        None
    };
    InputBandRow {
        mark: Some(mark),
        body,
        metadata: metadata.filter(|s| !s.is_empty()).map(|text| {
            Span::styled(
                text,
                Style::default()
                    .fg(color)
                    .underline_color(INPUT_DECORATION_MARKER),
            )
        }),
    }
    .into_line(width, Style::default())
}
fn compact_tool_line(
    language: UiLanguage,
    tool: &crate::presentation::ToolPresentation,
    width: u16,
    palette: theme::Palette,
) -> Line<'static> {
    let (kind, metadata, critical) = tool_metadata(language, tool, None);
    let mark = Span::styled(
        format!("{} ", kind.mark()),
        Style::default()
            .fg(kind.spec().2)
            .underline_color(INPUT_DECORATION_MARKER),
    );
    let description = crate::tool_description::label(tool.description.as_deref());
    let command =
        crate::presentation::sanitize_plain(description.as_deref().unwrap_or(&tool.command), 0)
            .0
            .replace(['\n', '\r'], " ");
    let command = if command.trim().is_empty() {
        "shell"
    } else {
        command.trim()
    };
    let body = Line::from(Span::styled(
        tool_text_fit(command, usize::from(width).saturating_sub(mark.width())),
        Style::default().fg(kind.spec().2),
    ));
    tool_row(
        mark,
        body,
        metadata,
        critical,
        width,
        kind.metadata_color(palette),
    )
}
fn tool_note(text: &str, width: u16, palette: theme::Palette) -> Vec<Line<'static>> {
    let safe = crate::presentation::sanitize_plain(text, 0).0;
    let style = Style::default()
        .fg(palette.muted())
        .underline_color(INPUT_DECORATION_MARKER);
    indented_wrap(vec![Line::from(safe)], width)
        .into_iter()
        .map(|mut line| {
            for span in &mut line.spans {
                span.style = style;
            }
            line
        })
        .collect()
}
fn append_item_rows(
    lines: &mut Vec<Line<'static>>,
    sources: &mut Vec<RowSource>,
    chunk: Vec<Line<'static>>,
    node: &UiNode,
    node_index: usize,
    region: ItemRegion,
    width: u16,
) {
    let covered_end = node_index + 1;
    let region = Arc::new(region);
    let mut byte = 0usize;
    let mut offset = node.base_char_offset;
    let mut excerpt_jump = false;
    let mut tail_search_budget = node.text.len();
    for line in hanging_wrap(chunk, width.max(1)) {
        // Raw display copies are not canonical text. Only exact saved substrings
        // advance original character offsets; decoration keeps the previous one.
        let plain = body_spans(&line)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let candidate = source_candidate(plain.trim_end());
        let gap_row = line
            .spans
            .iter()
            .any(|span| span.style.underline_color == Some(EXCERPT_GAP_MARKER));
        let output_row = line
            .spans
            .iter()
            .any(|span| span.style.underline_color == Some(OUTPUT_ROW_MARKER));
        if gap_row {
            excerpt_jump = true;
        }
        if !candidate.is_empty() && !gap_row {
            let end = node
                .text
                .floor_char_boundary((byte + 4096.max(candidate.len())).min(node.text.len()));
            let matched = node.text[byte..end]
                .find(candidate)
                .map(|found| (byte + found, candidate.len()))
                .or_else(|| {
                    if excerpt_jump && output_row {
                        excerpt_jump = false;
                        excerpt_tail_match(&node.text, candidate, byte, &mut tail_search_budget)
                    } else {
                        None
                    }
                });
            if let Some((start, length)) = matched {
                offset = node.base_char_offset + node.text[..start].chars().count();
                byte = start + length;
            }
        }
        sources.push(RowSource {
            node: node_index,
            covered_end,
            offset,
            item: Some(region.clone()),
        });
        lines.push(line);
    }
}
#[allow(clippy::too_many_arguments)]
fn append_node_cached(
    view: &ThreadView,
    index: usize,
    block_index: usize,
    node: &UiNode,
    start: usize,
    width: u16,
    links: &mut Vec<Arc<str>>,
    lines: &mut Vec<Line<'static>>,
    sources: &mut Vec<RowSource>,
    cache: &mut Option<&mut super::HistoryLayoutCache>,
    budget: usize,
) {
    let language = view.language;
    let key = ItemKey::Node(node.node_id.clone());
    let unit_key = cache.as_ref().and_then(|_| {
        super::RenderUnitKey::item(view, index, block_index, None, &key, width, links)
    });
    if let Some(hit) = unit_key
        .as_ref()
        .and_then(|key| cache.as_mut().and_then(|cache| cache.unit(key)))
    {
        lines.extend_from_slice(&hit.styled);
        sources.extend_from_slice(&hit.sources);
        *links = hit.targets.clone();
        return;
    }
    let row_start = lines.len();
    let slice = if block_index != usize::MAX {
        let mut copy = node.clone();
        copy.presentation.blocks = vec![node.presentation.blocks[block_index].clone()];
        copy.presentation.projection_stamp = None;
        Some(copy)
    } else {
        None
    };
    let node = slice.as_ref().unwrap_or(node);
    let mut chunk = if view.items.is_open(&key, true, "") {
        node_lines(
            language,
            node,
            view.reasoning_expanded.contains(&node.node_id),
            start,
            width,
            links,
        )
    } else {
        vec![Line::from(Text::Collapsed.render(language))]
    };
    user_band(language, &mut chunk, node, width, view.palette);
    append_item_rows(
        lines,
        sources,
        chunk,
        node,
        index,
        node_region(view, node),
        width,
    );
    if let (Some(key), Some(cache)) = (unit_key, cache.as_mut()) {
        cache.insert_unit(
            key,
            Arc::new(super::HistoryLayout {
                text: String::new(),
                styled: lines[row_start..].to_vec(),
                lines: lines.len() - row_start,
                targets: links.clone(),
                sources: sources[row_start..].to_vec(),
            }),
            budget,
        );
    }
}
pub(super) fn tool_observation_rank(
    block: &crate::presentation::PresentationBlock,
) -> (bool, bool) {
    (
        block.tool.as_ref().is_some_and(|tool| {
            tool.group_id
                .as_ref()
                .is_some_and(|group| !group.is_empty())
                && !tool.call_id.is_empty()
        }),
        block.kind == BlockKind::ToolResult,
    )
}
fn result_owners(view: &ThreadView, begin: usize, end: usize) -> BTreeMap<String, Option<ItemKey>> {
    let mut owners = BTreeMap::new();
    for node in view.nodes().iter().take(end).skip(begin) {
        for (index, block) in node.presentation.blocks.iter().enumerate() {
            if let Some(tool) = &block.tool
                && let Some(result) = &tool.result_node_id
            {
                let key = ItemKey::tool(tool, &node.node_id, index);
                if matches!(key, ItemKey::Tool { .. }) {
                    owners
                        .entry(result.clone())
                        .and_modify(|owner: &mut Option<ItemKey>| {
                            if owner.as_ref() != Some(&key) {
                                *owner = None;
                            }
                        })
                        .or_insert(Some(key));
                }
            }
        }
    }
    owners
}
fn correlated_tool_key(
    tool: &crate::presentation::ToolPresentation,
    node: &str,
    index: usize,
    owners: &BTreeMap<String, Option<ItemKey>>,
) -> ItemKey {
    let key = ItemKey::tool(tool, node, index);
    if matches!(key, ItemKey::LocalTool { .. })
        && let Some(Some(owner @ ItemKey::Tool { call_id, .. })) = owners.get(node)
        && call_id == &tool.call_id
    {
        owner.clone()
    } else {
        key
    }
}
fn append_notice_run(
    view: &ThreadView,
    begin: usize,
    end: usize,
    width: u16,
    lines: &mut Vec<Line<'static>>,
    sources: &mut Vec<RowSource>,
) {
    use crate::ui::text_width::glyphs;
    let nodes = &view.nodes()[begin..end];
    let first = &nodes[0];
    if nodes.len() == 1 {
        let symbol = notice_symbol(notice_kind(first));
        let prefix = format!("{} {symbol} ", RowKind::Notice.mark());
        append_item_rows(
            lines,
            sources,
            vec![notice_row(view, first, width, prefix)],
            first,
            begin,
            node_region(view, first),
            width,
        );
        return;
    }
    let key = ItemKey::NoticeGroup(first.node_id.clone());
    let open = view.items.is_open(&key, false, "");
    let mut counts = Vec::<(&str, usize)>::new();
    for node in nodes {
        let symbol = notice_symbol(notice_kind(node));
        if let Some((_, count)) = counts.iter_mut().find(|(seen, _)| *seen == symbol) {
            *count += 1;
        } else {
            counts.push((symbol, 1));
        }
    }
    let count_text = counts
        .into_iter()
        .map(|(symbol, count)| format!("{symbol} {}{count}", glyphs::select("×", "x")))
        .collect::<Vec<_>>()
        .join("  ");
    let title = format!(
        "{}{}{}  {count_text}",
        crate::ui::i18n::local!(view.language, "通知 ", ""),
        nodes.len(),
        crate::ui::i18n::local!(view.language, "件", " notices")
    );
    let action = if open {
        crate::ui::i18n::local!(view.language, "折り畳み", "Collapse")
    } else {
        crate::ui::i18n::local!(view.language, "展開", "Expand")
    };
    let prefix = format!("{} ", RowKind::Notice.mark());
    let (label, gap, right) =
        super::live_work::row_columns(usize::from(width), &prefix, &title, action, "");
    let header = Line::from(vec![
        Span::styled(prefix, Style::default().fg(theme::TEXT)),
        Span::raw(label),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(theme::MUTED)),
    ]);
    let region = ItemRegion {
        key: key.clone(),
        source_node_ids: nodes.iter().map(|node| node.node_id.clone()).collect(),
        primary_node_id: first.node_id.clone(),
        execution_id: None,
        expandable: true,
        default_open: false,
        outcome: String::new(),
    };
    append_item_rows(lines, sources, vec![header], first, begin, region, width);
    if let Some(source) = sources.last_mut() {
        source.covered_end = end;
    }
    if open {
        for (i, node) in nodes.iter().enumerate() {
            let selected = view.items.active
                && view.items.focus.as_ref() == Some(&ItemKey::Node(node.node_id.clone()));
            let symbol = notice_symbol(notice_kind(node));
            let prefix = glyphs::numbered_branch(i + 1 == nodes.len(), i + 1, selected, symbol);
            append_item_rows(
                lines,
                sources,
                vec![notice_row(view, node, width, prefix)],
                node,
                begin + i,
                node_region(view, node),
                width,
            );
        }
    }
}

fn typed_history(
    view: &ThreadView,
    begin: usize,
    end: usize,
    width: u16,
    mut cache: Option<&mut super::HistoryLayoutCache>,
    budget: usize,
) -> (Vec<Line<'static>>, Vec<Arc<str>>, Vec<RowSource>) {
    let language = view.language;
    let mut lines = Vec::new();
    let mut sources: Vec<RowSource> = Vec::new();
    let mut links = Vec::new();
    let owners = result_owners(view, begin, end);
    let mut best = BTreeMap::<ItemKey, (usize, usize)>::new();
    for (index, node) in view.nodes().iter().enumerate().take(end).skip(begin) {
        for (block_index, block) in node.presentation.blocks.iter().enumerate() {
            if let Some(tool) = &block.tool {
                let key = correlated_tool_key(tool, &node.node_id, block_index, &owners);
                if best.get(&key).is_none_or(|&(old_node, old_block)| {
                    tool_observation_rank(block)
                        >= tool_observation_rank(
                            &view.nodes()[old_node].presentation.blocks[old_block],
                        )
                }) {
                    best.insert(key, (index, block_index));
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut summary_until = begin;
    let mut notice_until = begin;
    for index in begin..end {
        if index < notice_until {
            continue;
        }
        if is_notice(&view.nodes()[index]) {
            notice_until = index + 1;
            while notice_until < end && is_notice(&view.nodes()[notice_until]) {
                notice_until += 1;
            }
            append_notice_run(view, index, notice_until, width, &mut lines, &mut sources);
            continue;
        }
        if index < summary_until {
            continue;
        }
        let node = &view.nodes()[index];
        if node.role == "context" {
            if let Some(last) = sources.last_mut() {
                last.covered_end = index + 1;
            }
            continue;
        }
        let before = lines.len();
        if node.presentation.blocks.is_empty()
            && (node.role == "tool" || node.presentation.kind == PresentationKind::ToolResult)
            && owners
                .get(&node.node_id)
                .and_then(Option::as_ref)
                .is_some_and(|owner| seen.contains(owner))
        {
            if let Some(last) = sources.last_mut() {
                last.covered_end = index + 1;
            }
            continue;
        }
        if let Some(summary) = interruption_group(&view.nodes()[index..end]) {
            let summary_index = index + summary;
            let summary_node = &view.nodes()[summary_index];
            let chunk = node_lines(
                language,
                summary_node,
                false,
                summary_node.base_char_offset,
                width,
                &mut links,
            );
            append_item_rows(
                &mut lines,
                &mut sources,
                chunk,
                summary_node,
                summary_index,
                node_region(view, summary_node),
                width,
            );
            summary_until = summary_index + 1;
            continue;
        }
        if node
            .presentation
            .blocks
            .iter()
            .any(|block| block.tool.is_some())
        {
            for (block_index, block) in node.presentation.blocks.iter().enumerate() {
                if let Some(tool) = &block.tool {
                    let key = correlated_tool_key(tool, &node.node_id, block_index, &owners);
                    if !seen.insert(key.clone()) {
                        // Exact canonical output stays at its own source ordinal,
                        // while sharing the one earlier header's item identity.
                        if block.kind != BlockKind::ToolResult || !block.source_exact {
                            continue;
                        }
                        let (best_node, best_block) = best[&key];
                        let canonical = view.nodes()[best_node].presentation.blocks[best_block]
                            .tool
                            .as_ref()
                            .expect("typed tool");
                        let outcome = tool_outcome(canonical);
                        if view
                            .items
                            .is_open(&key, canonical.requires_attention(), &outcome)
                            && view.items.preview_for(&key, &outcome).is_none()
                        {
                            let mut copy = node.clone();
                            copy.presentation.blocks = vec![block.clone()];
                            copy.presentation.blocks[0].tool = None;
                            let chunk = node_lines_with_header(
                                language,
                                &copy,
                                false,
                                block.source_start,
                                width,
                                &mut links,
                                false,
                            );
                            append_item_rows(
                                &mut lines,
                                &mut sources,
                                chunk,
                                node,
                                index,
                                ItemRegion {
                                    key,
                                    source_node_ids: vec![
                                        tool.call_node_id
                                            .clone()
                                            .unwrap_or_else(|| node.node_id.clone()),
                                        node.node_id.clone(),
                                    ],
                                    primary_node_id: node.node_id.clone(),
                                    execution_id: tool.execution_id.clone(),
                                    expandable: true,
                                    default_open: canonical.requires_attention(),
                                    outcome,
                                },
                                width,
                            );
                        }
                        continue;
                    }
                    let (source_index, source_block) = best[&key];
                    let tool = view.nodes()[source_index].presentation.blocks[source_block]
                        .tool
                        .as_ref()
                        .expect("typed tool");
                    let unit_key = cache.as_ref().and_then(|_| {
                        super::RenderUnitKey::item(
                            view,
                            index,
                            block_index,
                            Some(source_index),
                            &key,
                            width,
                            &links,
                        )
                    });
                    if let Some(hit) = unit_key
                        .as_ref()
                        .and_then(|key| cache.as_mut().and_then(|cache| cache.unit(key)))
                    {
                        lines.extend_from_slice(&hit.styled);
                        sources.extend_from_slice(&hit.sources);
                        links = hit.targets.clone();
                        continue;
                    }
                    let outcome = tool_outcome(tool);
                    let default_open = tool.requires_attention();
                    let open = view.items.is_open(&key, default_open, &outcome);
                    let mut source_ids = vec![];
                    for source in [&tool.call_node_id, &tool.result_node_id] {
                        if let Some(id) = source
                            && !source_ids.contains(id)
                        {
                            source_ids.push(id.clone());
                        }
                    }
                    if source_ids.is_empty() {
                        source_ids.push(node.node_id.clone());
                    }
                    let primary = tool
                        .result_node_id
                        .as_ref()
                        .or(tool.call_node_id.as_ref())
                        .unwrap_or(&node.node_id)
                        .clone();
                    let region = ItemRegion {
                        key: key.clone(),
                        source_node_ids: source_ids,
                        primary_node_id: primary,
                        execution_id: tool.execution_id.clone(),
                        expandable: true,
                        default_open,
                        outcome,
                    };
                    let row_start = lines.len();
                    let mut label = tool.clone();
                    if !view.describe_calls {
                        label.description = None;
                    }
                    let mut chunk = vec![compact_tool_line(language, &label, width, view.palette)];
                    if label.description.is_some() && open {
                        chunk.extend(tool_note(
                            Text::CommandPreview.render(language),
                            width,
                            view.palette,
                        ));
                        chunk.extend(tool_note(&tool.command, width, view.palette));
                    }
                    // Canonical call URLs remain original link targets after
                    // compacting the visible command.
                    annotate_links(&mut chunk, &node.presentation.links, &mut links);
                    let exact_body = &view.nodes()[source_index].presentation.blocks[source_block];
                    let deferred_exact = exact_body.source_exact
                        && source_index != index
                        && view.items.preview_for(&key, &region.outcome).is_none()
                        && !view.items.preview_failed(&key, &region.outcome);
                    if open && !deferred_exact {
                        if let Some(preview) = view.items.preview_for(&key, &region.outcome) {
                            let mut copy = node.clone();
                            copy.role = "tool".into();
                            copy.text = String::new();
                            copy.total_chars = 0;
                            copy.presentation.kind = PresentationKind::ToolResult;
                            copy.presentation.title = String::new();
                            copy.presentation.blocks = preview.blocks.clone();
                            for block in &mut copy.presentation.blocks {
                                block.tool = None;
                            }
                            chunk.extend(node_lines_with_header(
                                language, &copy, false, 0, width, &mut links, false,
                            ));
                            if let Some(notice) = &preview.notice {
                                chunk.extend(tool_note(notice, width, view.palette));
                            }
                            if let Some(error) = &preview.refresh_error {
                                chunk.extend(tool_note(
                                    &Message::RetryError {
                                        refresh: !preview.blocks.is_empty(),
                                        error: error.clone(),
                                    }
                                    .render(language),
                                    width,
                                    view.palette,
                                ));
                            } else if view.items.preview_loading(&key, &region.outcome) {
                                chunk.extend(tool_note(
                                    Text::Refreshing.render(language),
                                    width,
                                    view.palette,
                                ));
                            }
                        } else if view.items.preview_failed(&key, &region.outcome) {
                            chunk.extend(tool_note(
                                Text::OutputRetry.render(language),
                                width,
                                view.palette,
                            ));
                        } else if tool.execution_id.is_some()
                            && (default_open || view.items.preview_loading(&key, &region.outcome))
                        {
                            chunk.extend(tool_note(
                                Text::OutputLoading.render(language),
                                width,
                                view.palette,
                            ));
                        } else if exact_body.source_exact {
                            let mut copy = node.clone();
                            copy.presentation.blocks = vec![exact_body.clone()];
                            copy.presentation.blocks[0].tool = None;
                            chunk.extend(node_lines_with_header(
                                language,
                                &copy,
                                false,
                                exact_body.source_start,
                                width,
                                &mut links,
                                false,
                            ));
                        } else if default_open
                            && view.nodes()[source_index]
                                .presentation
                                .fields
                                .iter()
                                .any(|field| {
                                    field.key == format!("/ui_raw_stream_preview/{}", tool.call_id)
                                })
                        {
                            let source = &view.nodes()[source_index];
                            let mut copy = UiNode {
                                node_id: source.node_id.clone(),
                                role: "tool".into(),
                                presentation: crate::presentation::NodePresentation {
                                    kind: PresentationKind::ToolResult,
                                    blocks: source
                                        .presentation
                                        .blocks
                                        .iter()
                                        .filter(|block| {
                                            block.kind == BlockKind::ToolResult
                                                && block.stream.is_some()
                                                && block.tool.as_ref().is_some_and(|candidate| {
                                                    candidate.group_id == tool.group_id
                                                        && candidate.call_id == tool.call_id
                                                })
                                        })
                                        .map(|block| {
                                            let mut block = block.clone();
                                            block.tool = None;
                                            block
                                        })
                                        .collect(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            };
                            copy.presentation.links = source.presentation.links.clone();
                            chunk.extend(node_lines_with_header(
                                language, &copy, false, 0, width, &mut links, false,
                            ));
                            if let Some(note) = source.presentation.fields.iter().find(|field| {
                                field.key == format!("/ui_raw_stream_preview/{}", tool.call_id)
                            }) {
                                chunk.extend(tool_note(&note.value, width, view.palette));
                            }
                        } else if default_open
                            && view.nodes()[source_index]
                                .presentation
                                .fields
                                .iter()
                                .any(|field| {
                                    field.key
                                        == format!("/ui_raw_stream_preview_error/{}", tool.call_id)
                                })
                        {
                            chunk.extend(tool_note(
                                Text::OutputUnavailable.render(language),
                                width,
                                view.palette,
                            ));
                        } else if tool.execution_id.is_some() {
                            chunk.extend(tool_note(
                                if view.items.preview_loading(&key, &region.outcome) {
                                    Text::OutputLoading.render(language)
                                } else {
                                    Text::OutputEvicted.render(language)
                                },
                                width,
                                view.palette,
                            ));
                        } else if !tool.reason.is_empty() {
                            chunk.extend(tool_note(
                                &crate::presentation::messages::tool_reason(tool, language),
                                width,
                                view.palette,
                            ));
                        } else {
                            chunk.extend(tool_note(
                                Text::OutputOriginal.render(language),
                                width,
                                view.palette,
                            ));
                        }
                    }
                    append_item_rows(&mut lines, &mut sources, chunk, node, index, region, width);
                    if let (Some(key), Some(cache)) = (unit_key, cache.as_mut()) {
                        cache.insert_unit(
                            key,
                            Arc::new(super::HistoryLayout {
                                text: String::new(),
                                styled: lines[row_start..].to_vec(),
                                lines: lines.len() - row_start,
                                targets: links.clone(),
                                sources: sources[row_start..].to_vec(),
                            }),
                            budget,
                        );
                    }
                } else {
                    append_node_cached(
                        view,
                        index,
                        block_index,
                        node,
                        block.source_start,
                        width,
                        &mut links,
                        &mut lines,
                        &mut sources,
                        &mut cache,
                        budget,
                    );
                }
            }
        } else {
            append_node_cached(
                view,
                index,
                usize::MAX,
                node,
                node.base_char_offset,
                width,
                &mut links,
                &mut lines,
                &mut sources,
                &mut cache,
                budget,
            );
        }
        if lines.len() == before {
            // A duplicate tool result has zero rows. Only advance the already
            // traversed final frontier; never span over intervening prose.
            if let Some(last) = sources.last_mut() {
                last.covered_end = index + 1;
            }
        }
    }
    let (lines, sources) = row_boundaries(view, lines, sources);
    (lines, links, sources)
}

fn row_boundaries(
    view: &ThreadView,
    lines: Vec<Line<'static>>,
    sources: Vec<RowSource>,
) -> (Vec<Line<'static>>, Vec<RowSource>) {
    let mut output = Vec::with_capacity(lines.len());
    let mut mapped = Vec::with_capacity(sources.len());
    let mut previous: Option<(super::history_items::ItemKey, RowGroup)> = None;
    for (line, source) in lines.into_iter().zip(sources) {
        if let Some(item) = &source.item {
            let group = if matches!(item.key, ItemKey::Tool { .. } | ItemKey::LocalTool { .. }) {
                RowGroup::Tool
            } else {
                match view
                    .nodes()
                    .iter()
                    .find(|n| n.node_id == item.primary_node_id)
                    .unwrap_or(&view.nodes()[source.node])
                    .presentation
                    .kind
                {
                    PresentationKind::User => RowGroup::Input,
                    PresentationKind::Event | PresentationKind::Interrupted => RowGroup::Notice,
                    PresentationKind::Summary | PresentationKind::Spill => RowGroup::Replacement,
                    _ => RowGroup::Body,
                }
            };
            if previous
                .as_ref()
                .is_some_and(|(key, before)| key != &item.key && blank_between(*before, group))
            {
                let mut gap = source.clone();
                gap.item = None;
                output.push(Line::default());
                mapped.push(gap);
            }
            previous = Some((item.key.clone(), group));
        }
        output.push(line);
        mapped.push(source);
    }
    if let Some(last) = mapped.last().cloned() {
        let mut tail = last;
        tail.item = None;
        output.push(Line::default());
        mapped.push(tail);
    }
    (output, mapped)
}

fn excerpt_tail_match(
    source: &str,
    text: &str,
    byte: usize,
    budget: &mut usize,
) -> Option<(usize, usize)> {
    if text.is_empty() || *budget == 0 {
        return None;
    }
    let end = source.floor_char_boundary(byte.saturating_add(*budget).min(source.len()));
    let window = &source[byte..end];
    let found = window.find(text);
    let used = found.map_or(window.len(), |offset| offset + text.len());
    *budget = budget.saturating_sub(used);
    found.map(|offset| (byte + offset, text.len()))
}
pub(super) fn plain(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
pub(super) fn bytes(lines: &Vec<Line<'static>>) -> usize {
    lines.capacity() * std::mem::size_of::<Line<'static>>()
        + lines
            .iter()
            .map(|l| {
                l.spans.capacity() * std::mem::size_of::<Span<'static>>()
                    + l.spans
                        .iter()
                        .map(|s| match &s.content {
                            std::borrow::Cow::Owned(text) => text.capacity(),
                            std::borrow::Cow::Borrowed(text) => text.len(),
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
}
pub(super) fn borrowed<'a>(lines: &'a [Line<'static>]) -> Vec<Line<'a>> {
    lines
        .iter()
        .map(|l| Line {
            style: l.style,
            alignment: l.alignment,
            spans: l
                .spans
                .iter()
                .map(|s| {
                    Span::styled(
                        s.content.as_ref(),
                        Style {
                            underline_color: None,
                            ..s.style
                        },
                    )
                })
                .collect(),
        })
        .collect()
}

/// Paint live seconds over cached rows; no source/layout revision or DB read.
pub(super) fn paint_live_times(
    lines: &mut [Line<'_>],
    cached: &[Line<'static>],
    sources: &[RowSource],
    view: &ThreadView,
    width: u16,
    now: std::time::Instant,
) {
    let language = view.language;
    if !lines.iter().any(|line| {
        line.spans
            .first()
            .is_some_and(|s| s.content.starts_with(RowKind::Running.mark()))
    }) {
        return;
    }
    for ((line, cached), source) in lines.iter_mut().zip(cached).zip(sources) {
        if !line
            .spans
            .first()
            .is_some_and(|s| s.content.starts_with(RowKind::Running.mark()))
        {
            continue;
        }
        let Some(tool) = source.item.as_ref().and_then(|item| {
            view.nodes()
                .get(source.node)?
                .presentation
                .blocks
                .iter()
                .filter_map(|block| block.tool.as_ref())
                .find(|tool| tool.execution_id == item.execution_id)
        }) else {
            continue;
        };
        let Some(start) = tool.elapsed_observation else {
            continue;
        };
        let seconds = (start.0 + now.saturating_duration_since(start.1)).as_secs();
        let Some(mark) = cached.spans.first().cloned() else {
            continue;
        };
        let body = Line::from(
            body_spans(cached)
                .map(|span| Span::styled(span.content.to_string(), span.style))
                .collect::<Vec<_>>(),
        );
        let (kind, fallback, critical) = tool_metadata(language, tool, Some(seconds));
        let metadata = cached
            .spans
            .last()
            .filter(|span| {
                span.style.underline_color == Some(INPUT_DECORATION_MARKER)
                    && !span.content.trim().is_empty()
            })
            .map(|span| {
                let mut parts = span
                    .content
                    .split(crate::ui::text_width::glyphs::separator())
                    .collect::<Vec<_>>();
                if parts.last().is_some_and(|part| {
                    *part == Text::ElapsedUnavailable.render(language)
                        || (part.ends_with('秒')
                            || part.ends_with('分')
                            || (language == UiLanguage::En && part.ends_with('s')))
                }) {
                    parts.pop();
                }
                let mut value = parts.join(crate::ui::text_width::glyphs::separator());
                if !value.is_empty() {
                    value.push_str(crate::ui::text_width::glyphs::separator());
                }
                value.push_str(
                    &Message::Elapsed {
                        ms: seconds.saturating_mul(1000),
                        decimal: false,
                    }
                    .render(language),
                );
                value
            })
            .unwrap_or(fallback);
        let mut painted = tool_row(
            mark,
            body,
            metadata,
            critical,
            width,
            kind.metadata_color(view.palette),
        );
        for span in &mut painted.spans {
            span.style.underline_color = None;
        }
        *line = painted;
    }
}

/// Use Ratatui's identical word wrapper offscreen, once per layout cache miss.
/// Marker styles never enter the terminal Buffer or canonical text.
pub(super) fn link_regions(
    lines: &[Line<'static>],
    targets: &[std::sync::Arc<str>],
    width: u16,
    height: u16,
    scroll: u16,
) -> Vec<crate::terminal_links::LinkRegion> {
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        widgets::{Paragraph, Widget, Wrap},
    };
    if targets.is_empty() || width == 0 || height == 0 {
        return vec![];
    }
    let marked = lines
        .iter()
        .map(|line| {
            Line::from(
                line.spans
                    .iter()
                    .map(|span| {
                        let marker = span.style.underline_color.unwrap_or(Color::Reset);
                        Span::styled(span.content.as_ref(), Style::default().fg(marker))
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(marked)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .render(area, &mut buffer);
    crate::terminal_links::collect_regions(&buffer, area, targets, |cell| cell.fg)
}

#[cfg(test)]
fn japanese_config() -> crate::config::Config {
    let mut config = crate::config::Config::default();
    config.ui.language = UiLanguage::Ja;
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_and_missing_notice_kinds_use_the_approved_unknown_mark() {
        crate::ui::text_width::set_terminal_width(Some(1));
        assert_eq!(notice_symbol(None), RowKind::Unknown.mark());
        assert_eq!(notice_symbol(Some("future_notice")), "◌");
        assert_eq!(notice_symbol(Some("shell_ended")), "$");
        for language in [UiLanguage::Ja, UiLanguage::En] {
            let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
            view.language = language;
            let event = |id: &str, kind: Option<&str>| UiNode {
                node_id: id.into(),
                role: "event".into(),
                text: format!("raw-{id}"),
                presentation: crate::presentation::NodePresentation {
                    kind: PresentationKind::Event,
                    ui_messages: crate::presentation::messages::PresentationMessages {
                        title: kind.map(|kind| Message::EventTitle { kind: kind.into() }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            };
            *view.nodes_mut_for_test() = vec![
                event("missing", None),
                event("future", Some("future_notice")),
            ];
            view.total_nodes = 2;
            for width in [80, 120] {
                let (lines, _, sources) = history(&view, width);
                assert!(plain(&lines).contains("◌"));
                assert!(matches!(
                    sources[0].item.as_ref().unwrap().key,
                    ItemKey::NoticeGroup(_)
                ));
                view.items
                    .toggle(ItemKey::NoticeGroup("missing".into()), false, "");
                let (lines, _, sources) = history(&view, width);
                let text = plain(&lines);
                assert!(
                    text.contains("├─[1] ◌") && text.contains("└─[2] ◌"),
                    "{text}"
                );
                assert!(sources.iter().any(|s| {
                    s.item
                        .as_ref()
                        .is_some_and(|i| i.key == ItemKey::Node("future".into()))
                }));
                view.items
                    .toggle(ItemKey::NoticeGroup("missing".into()), false, "");
            }
            assert_eq!(view.nodes()[0].text, "raw-missing");
            assert_eq!(view.nodes()[1].text, "raw-future");
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn shell_notice_uses_description_setting_signal_and_localized_elapsed() {
        let node = UiNode {
            node_id: "one".into(),
            presentation: crate::presentation::NodePresentation {
                kind: PresentationKind::Event,
                fields: vec![
                    crate::presentation::PresentationField {
                        key: "/ui_notice/description".into(),
                        value: "check tests".into(),
                        ..Default::default()
                    },
                    crate::presentation::PresentationField {
                        key: "/payload/exit_signal".into(),
                        value: "15".into(),
                        ..Default::default()
                    },
                ],
                ui_messages: crate::presentation::messages::PresentationMessages {
                    title: Some(Message::EventIdentity {
                        kind: "shell_ended".into(),
                        identity: "secret-id".into(),
                        status: Some("stopped".into()),
                        code: None,
                        elapsed_ms: Some(149000),
                    }),
                    notice: Some(Message::CommandNotice {
                        command: Some("cargo test".into()),
                        background: None,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        for language in [UiLanguage::Ja, UiLanguage::En] {
            for width in [80, 120] {
                let mut view =
                    ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
                view.language = language;
                view.describe_calls = true;
                let line = notice_row(&view, &node, width, "⁂ $ ".into());
                let text = plain(&[line]);
                assert!(
                    text.contains("check tests") && text.contains("SIGTERM"),
                    "{text}"
                );
                assert!(!text.contains("secret-id") && !text.contains("cargo test"));
                assert!(
                    text.contains(if language == UiLanguage::Ja {
                        "2分29秒"
                    } else {
                        "2m29s"
                    }),
                    "{text}"
                );
                view.describe_calls = false;
                assert!(
                    plain(&[notice_row(&view, &node, width, "⁂ $ ".into())]).contains("cargo test")
                );
                view.describe_calls = true;
                view.items.active = true;
                view.items.focus = Some(ItemKey::Node("one".into()));
                assert!(
                    plain(&[notice_row(&view, &node, width, "⁂ $ ".into())]).contains("cargo test")
                );
            }
        }
    }

    #[test]
    fn shell_exit_and_unknown_signal_are_localized() {
        for language in [UiLanguage::Ja, UiLanguage::En] {
            let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
            view.language = language;
            let mut node = UiNode {
                presentation: crate::presentation::NodePresentation {
                    kind: PresentationKind::Event,
                    ui_messages: crate::presentation::messages::PresentationMessages {
                        title: Some(Message::EventIdentity {
                            kind: "shell_ended".into(),
                            identity: "id".into(),
                            status: Some("completed".into()),
                            code: Some(0),
                            elapsed_ms: None,
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            };
            let shown = plain(&[notice_row(&view, &node, 120, "".into())]);
            assert!(shown.contains("exit 0"), "{shown}");
            assert!(
                !shown.contains("Result saved") && !shown.contains("結果あり"),
                "{shown}"
            );
            node.presentation
                .fields
                .push(crate::presentation::PresentationField {
                    key: "/payload/exit_signal".into(),
                    value: "99".into(),
                    ..Default::default()
                });
            let shown = plain(&[notice_row(&view, &node, 120, "".into())]);
            assert!(
                shown.contains(if language == UiLanguage::Ja {
                    "シグナル 99"
                } else {
                    "signal 99"
                }),
                "{shown}"
            );
        }
    }

    #[test]
    fn notification_group_preserves_individual_sources_and_measured_rows() {
        use crate::ui::text_width::UnicodeWidthStr;
        fn event(id: &str, kind: &str) -> UiNode {
            UiNode {
                node_id: id.into(),
                text: format!("raw-{id}"),
                total_chars: 5,
                presentation: crate::presentation::NodePresentation {
                    kind: PresentationKind::Event,
                    ui_messages: crate::presentation::messages::PresentationMessages {
                        title: Some(Message::EventTitle { kind: kind.into() }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            }
        }
        for width in [80, 120] {
            for language in [UiLanguage::Ja, UiLanguage::En] {
                crate::ui::text_width::set_terminal_width(Some(1));
                let mut view =
                    ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
                view.language = language;
                *view.nodes_mut_for_test() =
                    vec![event("one", "run_started"), event("two", "index_state")];
                view.total_nodes = 2;
                let (lines, _, sources) = history(&view, width);
                assert_eq!(sources.iter().filter(|s| s.item.is_some()).count(), 1);
                assert!(plain(&lines).contains("⁂"));
                let header = plain(&lines);
                assert!(
                    header.find("⟡ ×1").unwrap() < header.find("⌕ ×1").unwrap(),
                    "{header}"
                );
                assert!(
                    lines
                        .iter()
                        .all(|line| plain(std::slice::from_ref(line)).width() <= width as usize)
                );
                assert_eq!(sources[0].covered_end, 2);
                view.items
                    .toggle(ItemKey::NoticeGroup("one".into()), false, "");
                let (lines, _, sources) = history(&view, width);
                let text = plain(&lines);
                assert!(
                    text.contains("├─[1] ⟡") && text.contains("└─[2] ⌕"),
                    "{text}"
                );
                assert!(sources.iter().any(|s| {
                    s.item
                        .as_ref()
                        .is_some_and(|i| i.key == ItemKey::Node("one".into()))
                }));
                assert!(sources.iter().any(|s| {
                    s.item
                        .as_ref()
                        .is_some_and(|i| i.key == ItemKey::Node("two".into()))
                }));
                assert!(
                    lines
                        .iter()
                        .all(|line| plain(std::slice::from_ref(line)).width() <= width as usize)
                );
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }
    #[test]
    fn formal_shutdown_group_identity_survives_generic_field_limits() {
        fn scene(value: &serde_json::Value, summary_kind: &str) -> Vec<UiNode> {
            let mut nodes = Vec::new();
            for section in [
                "runs",
                "turns",
                "shells",
                "owned_children",
                "pending_operations_with_unknown_targets",
            ] {
                let saved = &value["change"][section];
                let payload = serde_json::json!({"text":serde_json::json!({"section":section,"items":saved["items"]}).to_string()});
                let text = crate::history::text_of_payload(&payload);
                nodes.push(UiNode {
                    node_id: saved["detail_node_ids"][0].as_str().unwrap().into(),
                    text: text.clone(),
                    total_chars: text.chars().count(),
                    presentation: crate::presentation::project(
                        "interruption_details",
                        &payload,
                        &text,
                        0,
                    ),
                    ..Default::default()
                });
            }
            let payload = serde_json::json!({"text":value.to_string()});
            let text = crate::history::text_of_payload(&payload);
            let summary = crate::presentation::project(summary_kind, &payload, &text, 0);
            assert!(summary.fields.len() <= 128);
            assert!(summary.metadata_bytes() <= crate::presentation::MAX_METADATA_BYTES);
            nodes.push(UiNode {
                node_id: "summary".into(),
                text: text.clone(),
                total_chars: text.chars().count(),
                presentation: summary,
                ..Default::default()
            });
            nodes
        }
        // Anonymous shape of the actual five-section shutdown snapshot. The
        // outer /text field tree plus decoded envelope exceeds 128 fields.
        let small:serde_json::Value=serde_json::from_str(r#"{"actor":{"kind":"shellboy","source":"shutdown"},"change":{"automatic_replay":false,"cancelled_waits":{"count":0,"detail_node_count":0,"detail_node_ids":[],"details_kind":"interruption_details","history_kind":"fixture","items":[],"section":"cancelled_waits","truncated":false},"pending_operations_with_unknown_targets":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-00"],"details_kind":"interruption_details","items":[{"operation_id":"fixture","request_kind":"fixture","state":"fixture","wait_targets":"fixture"}],"section":"pending_operations_with_unknown_targets","truncated":false},"os_processes_inspected":false,"owned_children":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-01"],"details_kind":"interruption_details","items":[{"end_message_node_id":"fixture-id-02","first_message_id":"fixture-id-03","first_request_node_id":"fixture-id-04","last_turn_id":"fixture-id-05","owner_run_id":"fixture-id-06","planned_state":"stopped","previous_state":"live","run_id":"fixture-id-07","thread_id":"fixture-id-08"}],"section":"owned_children","truncated":false},"reason":"app_shutdown","result_uncertain":true,"runs":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-09"],"details_kind":"interruption_details","items":[{"first_message_id":"fixture-id-10","first_request_node_id":"fixture-id-11","owner_run_id":null,"planned_state":"stopped","previous_state":"live","request_count":1,"run_id":"fixture-id-06","stop_reason":"app_shutdown"}],"section":"runs","truncated":false},"shells":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-12"],"details_kind":"interruption_details","items":[{"execution_id":"fixture-id-13","output_complete":false,"planned_state":"stopped","previous_state":"running","result_uncertain":true,"retention_generation":0,"run_id":"fixture-id-06","saved_sequence":1,"turn_id":"fixture-id-14"}],"section":"shells","truncated":false},"turns":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-15"],"details_kind":"interruption_details","items":[{"end_message_node_id":null,"previous_state":"running","recovery_reason":null,"run_id":"fixture-id-06","turn_id":"fixture-id-14"}],"section":"turns","truncated":false},"unreflected_messages":{"count":0,"detail_node_count":0,"detail_node_ids":[],"details_kind":"interruption_details","items":[],"section":"unreflected_messages","truncated":false}},"kind":"app_shutdown","observed_at":"fixture","snapshot_node_id":"fixture-id-16","timezone":"fixture"}"#).unwrap();
        let generic = crate::presentation::fields_of_value(&small);
        let wrapped =
            crate::presentation::fields_of_value(&serde_json::json!({"text":small.to_string()}));
        assert!(generic.len() + wrapped.len() > 128);
        let nodes = scene(&small, "interruption_context");
        assert_eq!(interruption_group(&nodes), Some(5));
        assert!(
            nodes
                .last()
                .unwrap()
                .presentation
                .fields
                .iter()
                .any(|field| field.key == "/kind" && field.value == "app_shutdown")
        );
        let mut large = small.clone();
        let template = large["change"]["shells"]["items"][0].clone();
        large["change"]["shells"]["items"] = serde_json::Value::Array(
            (0..32)
                .map(|index| {
                    let mut item = template.clone();
                    item["execution_id"] = serde_json::json!(format!("fixture-shell-{index}"));
                    item
                })
                .collect(),
        );
        large["change"]["shells"]["count"] = serde_json::json!(32);
        assert_eq!(
            crate::presentation::fields_of_value(&large).len(),
            128,
            "one decoded envelope also reaches the generic limit"
        );
        assert_eq!(
            interruption_group(&scene(&large, "interruption_context")),
            Some(5)
        );
        assert_eq!(interruption_group(&scene(&small, "tool_result")), None);
        let mut unknown_kind = small.clone();
        unknown_kind["kind"] = serde_json::json!("future_shutdown");
        assert_eq!(
            interruption_group(&scene(&unknown_kind, "interruption_context")),
            None
        );
        let mut unknown_path = small.clone();
        let refs = unknown_path["change"]["runs"]
            .as_object_mut()
            .unwrap()
            .remove("detail_node_ids")
            .unwrap();
        unknown_path["change"]["runs"]["nested"] = serde_json::json!({"detail_node_ids":refs});
        // Keep the detail's ID available to build the fixture, then remove the
        // canonical identity from the projected summary alone.
        let mut invalid = scene(&small, "interruption_context");
        let payload = serde_json::json!({"text":unknown_path.to_string()});
        let text = crate::history::text_of_payload(&payload);
        invalid.last_mut().unwrap().presentation =
            crate::presentation::project("interruption_context", &payload, &text, 0);
        assert_eq!(interruption_group(&invalid), None);
        let mut unknown_section = scene(&small, "interruption_context");
        let section = unknown_section[0]
            .presentation
            .fields
            .iter_mut()
            .find(|field| field.key == "/section")
            .unwrap();
        section.value = "future_sources".into();
        assert_eq!(interruption_group(&unknown_section), None);
    }

    #[test]
    fn a_generated_gap_never_matches_the_same_text_inside_omitted_stdout() {
        use crate::presentation::{NodePresentation, PresentationBlock};
        let gap = ". 出力抜粋（表示120行中115行省略）| F6 詳細";
        let body = (0..120)
            .map(|index| {
                if index == 50 {
                    gap.to_owned()
                } else if index == 2 {
                    "| LINE002".to_owned()
                } else {
                    format!("LINE{index:03}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let node = UiNode {
            node_id: "stdout-source".into(),
            role: "tool".into(),
            text: body.clone(),
            total_chars: body.chars().count(),
            presentation: NodePresentation {
                kind: PresentationKind::ToolResult,
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    text: body.clone(),
                    source_start: 0,
                    source_end: body.chars().count(),
                    source_exact: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![node];
        let (lines, _, sources) = history(&view, 100);
        let head_row = lines
            .iter()
            .position(|line| plain(std::slice::from_ref(line)).contains("LINE002"))
            .unwrap();
        let head_offset = body[..body.find("| LINE002").unwrap()].chars().count();
        assert_eq!(sources[head_row].offset, head_offset);
        let gap_rows = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                line.spans
                    .iter()
                    .any(|span| span.style.underline_color == Some(EXCERPT_GAP_MARKER))
            })
            .map(|(row, _)| row)
            .collect::<Vec<_>>();
        assert!(!gap_rows.is_empty());
        assert!(
            gap_rows
                .iter()
                .all(|row| sources[*row].offset == head_offset),
            "generated gap rows retain the preceding real source instead of jumping to line 50"
        );
        let tail_row = lines
            .iter()
            .position(|line| plain(std::slice::from_ref(line)).contains("LINE118"))
            .unwrap();
        let tail_offset = body[..body.find("LINE118").unwrap()].chars().count();
        assert_eq!(
            sources[tail_row].offset, tail_offset,
            "the real tail still gets its exact source position"
        );
    }

    #[test]
    fn excerpt_gap_metadata_survives_wrapping_without_becoming_a_terminal_link() {
        let gap = Line::from(Span::styled(
            ". 出力抜粋（表示120行中115行省略）| F6 詳細",
            Style::default()
                .fg(theme::MUTED)
                .underline_color(EXCERPT_GAP_MARKER),
        ));
        let mut rows = hanging_wrap(vec![gap], 16);
        assert!(rows.len() > 1);
        for row in &mut rows {
            row.spans
                .insert(0, Span::styled("  | ", Style::default().fg(theme::MUTED)));
        }
        let rows = hanging_wrap(rows, 20);
        assert!(rows.iter().all(|row| {
            row.spans
                .iter()
                .any(|span| span.style.underline_color == Some(EXCERPT_GAP_MARKER))
        }));
        assert!(
            borrowed(&rows)
                .iter()
                .flat_map(|row| &row.spans)
                .all(|span| span.style.underline_color.is_none())
        );
        assert!(
            link_regions(
                &rows,
                &[std::sync::Arc::from("https://example.invalid/not-gap")],
                20,
                rows.len() as u16,
                0
            )
            .is_empty()
        );
    }

    #[test]
    fn typed_output_guides_preserve_rgb_links_and_unavailable_target_markers() {
        let url = "https://example.invalid/output";
        let rows = vec![Line::from(vec![
            Span::styled(
                "| ",
                Style::default()
                    .fg(theme::MUTED)
                    .underline_color(OUTPUT_ROW_MARKER),
            ),
            Span::styled(
                url,
                Style::default()
                    .fg(theme::TEXT)
                    .underline_color(crate::terminal_links::marker_color(0)),
            ),
            Span::styled(
                " unavailable",
                Style::default().underline_color(Color::Reset),
            ),
        ])];
        let regions = link_regions(&rows, &[std::sync::Arc::from(url)], 100, 1, 0);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].url.as_ref(), url);
        assert_eq!(
            regions[0].x, 2,
            "the typed guide must never become part of the clickable URL"
        );
        assert!(
            borrowed(&rows)
                .iter()
                .flat_map(|row| &row.spans)
                .all(|span| span.style.underline_color.is_none())
        );
    }

    #[test]
    fn wrapped_group_headers_cannot_consume_matching_stdout_source() {
        use crate::presentation::{NodePresentation, PresentationBlock, ToolPresentation};
        let token = "MATCH_BODY_TARGET123";
        assert_eq!(token.len(), 20);
        let body = format!("{token}\n{token}\nTAIL\n");
        let tool = ToolPresentation {
            group_id: Some("fixture-group".into()),
            call_node_id: Some("call".into()),
            result_node_id: Some("result".into()),
            call_id: "c".into(),
            ordinal: Some(0),
            command: format!("{} {token}", "prefix ".repeat(8)),
            execution_id: Some("e".into()),
            status: "exited".into(),
            exit_code: Some(0),
            ..Default::default()
        };
        let call = UiNode {
            node_id: "call".into(),
            role: "assistant".into(),
            text: "call source".into(),
            total_chars: 11,
            presentation: NodePresentation {
                kind: PresentationKind::Assistant,
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(tool.clone()),
                    text: "call source".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = UiNode {
            node_id: "result".into(),
            text: body.clone(),
            base_char_offset: 123,
            total_chars: 123 + body.chars().count(),
            presentation: NodePresentation {
                kind: PresentationKind::ToolResult,
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    tool: Some(tool),
                    text: body,
                    source_start: 123,
                    source_end: 170,
                    source_exact: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![call, result];
        let tool = view.nodes()[1].presentation.blocks[0]
            .tool
            .as_ref()
            .unwrap();
        let key = ItemKey::tool(tool, "result", 0);
        let outcome = tool_outcome(tool);
        view.items.toggle(key, false, &outcome);
        let (lines, _, sources) = history(&view, 22);
        let first_body = lines
            .iter()
            .position(|line| {
                line.spans.iter().any(|span| {
                    span.style.fg == Some(theme::TEXT) && span.content.contains("MATCH_BODY")
                })
            })
            .unwrap();
        assert_eq!(
            lines[..first_body]
                .iter()
                .filter(|line| !plain(std::slice::from_ref(line)).trim().is_empty())
                .count(),
            1,
            "the command occupies one compact row"
        );
        assert_eq!(sources[0].node, 0);
        assert!(
            sources[..first_body].iter().all(|source| source.node == 0),
            "header decoration keeps call anchors"
        );
        assert_eq!(sources[first_body].node, 1);
        let repeated = lines
            .iter()
            .enumerate()
            .skip(first_body + 1)
            .find(|(_, line)| {
                line.spans.iter().any(|span| {
                    span.style.fg == Some(theme::TEXT) && span.content.contains("MATCH_BODY")
                })
            })
            .unwrap()
            .0;
        assert_eq!(
            sources[repeated].offset,
            123 + token.chars().count() + 1,
            "repeated body token advances only on actual output"
        );
        assert_eq!(
            sources[first_body].offset, 123,
            "matching header continuation must not advance past the first stdout token"
        );
    }

    #[test]
    fn tail_source_search_has_one_window_budget_and_utf8_boundaries() {
        let source = format!("{}尾部🙂", "日本語".repeat(3000));
        let mut budget = source.len();
        let found = excerpt_tail_match(&source, "尾部🙂", 0, &mut budget).unwrap();
        assert_eq!(&source[found.0..found.0 + found.1], "尾部🙂");
        assert_eq!(budget, 0);
        assert!(
            excerpt_tail_match(&source, "日本語", 0, &mut budget).is_none(),
            "decoration/repeated candidates cannot restart a whole-window scan"
        );
        let mut tiny = 1;
        assert!(excerpt_tail_match(&source, "日本語", 0, &mut tiny).is_none());
    }

    #[test]
    fn grouped_unicode_bodies_keep_their_real_node_offsets_and_window_requests() {
        use crate::presentation::{
            NodePresentation, PresentationBlock, PresentationField, ToolPresentation,
        };
        let field = |key: &str, value: &str| PresentationField {
            key: key.into(),
            label: key.into(),
            value: value.into(),
            display: None,
            value_omitted: false,
        };
        let body = (0..120)
            .map(|i| format!("行{i:03} 日本語🙂 e\u{301} bounded body\n"))
            .collect::<String>();
        let base = 65_000;
        for interruption in [false, true] {
            let mut leading = UiNode {
                node_id: "first".into(),
                text: "unrelated original request".into(),
                total_chars: 26,
                ..Default::default()
            };
            let actual = if interruption { "summary" } else { "result" };
            let tool = ToolPresentation {
                group_id: Some("fixture-group".into()),
                call_node_id: Some("first".into()),
                result_node_id: Some(actual.into()),
                call_id: "c".into(),
                ordinal: Some(0),
                command: "echo grouped".into(),
                execution_id: Some("execution".into()),
                status: "exited".into(),
                exit_code: Some(0),
                ..Default::default()
            };
            if interruption {
                leading.presentation = NodePresentation {
                    kind: PresentationKind::Interrupted,
                    fields: vec![field("/section", "runs")],
                    ..Default::default()
                };
            } else {
                leading.role = "assistant".into();
                leading.presentation = NodePresentation {
                    kind: PresentationKind::Assistant,
                    blocks: vec![PresentationBlock {
                        kind: BlockKind::ToolCall,
                        tool: Some(tool.clone()),
                        text: leading.text.clone(),
                        ..Default::default()
                    }],
                    ..Default::default()
                };
            }
            let rendered = UiNode {
                node_id: actual.into(),
                text: body.clone(),
                base_char_offset: base,
                total_chars: base + body.chars().count() + 2000,
                presentation: NodePresentation {
                    kind: if interruption {
                        PresentationKind::Interrupted
                    } else {
                        PresentationKind::ToolResult
                    },
                    title: if interruption {
                        "前回の中断記録".into()
                    } else {
                        String::new()
                    },
                    fields: if interruption {
                        vec![
                            field("/kind", "previous_instance_interrupted"),
                            field("/change/runs/detail_node_ids/0", "first"),
                        ]
                    } else {
                        vec![]
                    },
                    blocks: vec![PresentationBlock {
                        kind: if interruption {
                            BlockKind::Event
                        } else {
                            BlockKind::ToolResult
                        },
                        tool: (!interruption).then_some(tool),
                        text: body.clone(),
                        source_start: base,
                        source_end: base + body.chars().count(),
                        source_exact: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
            view.thread_id = Some("thread".into());
            *view.nodes_mut_for_test() = vec![leading, rendered];
            view.total_nodes = 2;
            view.navigation.select_reading();
            if !interruption {
                let tool = view.nodes()[1].presentation.blocks[0]
                    .tool
                    .as_ref()
                    .unwrap();
                let key = ItemKey::tool(tool, actual, 0);
                let outcome = tool_outcome(tool);
                view.items.toggle(key, false, &outcome);
            }
            let (lines, _, sources) = history(&view, 100);
            let target = if interruption { "行040" } else { "行118" };
            let next = if interruption { "行041" } else { "行119" };
            let row = lines
                .iter()
                .position(|line| plain(std::slice::from_ref(line)).contains(target))
                .unwrap();
            let expected = base + body[..body.find(target).unwrap()].chars().count();
            assert_eq!(
                sources[0].node, 0,
                "only the generated group header owns the first member"
            );
            assert_eq!(
                sources[row].node, 1,
                "body belongs to its rendered result/summary"
            );
            assert_eq!(
                sources[row].offset, expected,
                "original Unicode position is preserved"
            );
            assert_eq!(
                sources.last().unwrap().node,
                1,
                "trailing blank follows the body source"
            );
            view.navigation.anchor = crate::ui::ViewAnchor {
                node_id: Some(actual.into()),
                char_offset: expected,
            };
            let mut cache = super::super::HistoryLayoutCache::default();
            let budget = 1024 * 1024;
            let layout = view.history_layout("fixture", &mut cache, budget, 100, 2);
            view.project_viewport(&layout, 100, 2);
            assert!(view.scroll(1, "fixture", &mut cache, budget).is_empty());
            assert_eq!(view.navigation.anchor.node_id.as_deref(), Some(actual));
            let expected_next = base + body[..body.find(next).unwrap()].chars().count();
            assert_eq!(view.navigation.anchor.char_offset, expected_next);
            let mut requested = false;
            for _ in 0..150 {
                for action in view.scroll(1, "fixture", &mut cache, budget) {
                    if let crate::ui::UiAction::LoadNodeWindow {
                        node_id,
                        char_offset,
                        ..
                    } = action
                    {
                        assert_eq!(
                            node_id, actual,
                            "next body window must never target the leading call/detail"
                        );
                        assert!(char_offset >= base);
                        requested = true;
                    }
                }
                if requested {
                    break;
                }
            }
            assert!(
                requested,
                "the bounded Unicode body reaches a source-window boundary"
            );
        }
    }

    #[test]
    fn multiple_calls_show_requests_separately_from_terminal_result_facts() {
        use crate::presentation::{NodePresentation, PresentationBlock, ToolPresentation};
        let mut call = UiNode {
            node_id: "multi-call".into(),
            role: "assistant".into(),
            text: "source calls".into(),
            total_chars: 12,
            presentation: NodePresentation {
                kind: PresentationKind::Assistant,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut results = Vec::new();
        for (ordinal, command, exit) in [(0, "echo first", 0), (1, "echo second", 7)] {
            let tool = ToolPresentation {
                group_id: Some("fixture-group".into()),
                call_node_id: Some("multi-call".into()),
                result_node_id: Some(format!("result-{ordinal}")),
                call_id: format!("call-{ordinal}"),
                ordinal: Some(ordinal),
                command: command.into(),
                status: "completed".into(),
                current_state: Some("exited".into()),
                current_exit_code: Some(exit),
                execution_id: Some(format!("e-{ordinal}")),
                ..Default::default()
            };
            call.presentation.blocks.push(PresentationBlock {
                kind: BlockKind::ToolCall,
                tool: Some(tool.clone()),
                text: command.into(),
                ..Default::default()
            });
            results.push(UiNode {
                node_id: format!("result-{ordinal}"),
                text: format!("output-{ordinal}"),
                total_chars: 8,
                presentation: NodePresentation {
                    kind: PresentationKind::ToolResult,
                    blocks: vec![PresentationBlock {
                        kind: BlockKind::ToolResult,
                        tool: Some(tool),
                        text: format!("output-{ordinal}"),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![call];
        view.nodes_mut_for_test().extend(results);
        let shown = plain(&history(&view, 120).0);
        assert_eq!(shown.matches("echo first").count(), 1);
        assert_eq!(shown.matches("echo second").count(), 1);
        assert_eq!(shown.matches("exit 0").count(), 1);
        assert_eq!(shown.matches("exit 7").count(), 1);
        assert!(!shown.contains("#1") && !shown.contains("#2"));
        let (_, _, sources) = history(&view, 120);
        let tools = sources
            .iter()
            .filter_map(|row| row.item.as_ref())
            .filter(|item| matches!(item.key, ItemKey::Tool { .. }))
            .map(|item| item.key.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            tools.len(),
            2,
            "each saved group/call has exactly one presentation item"
        );
        assert_eq!(view.nodes().len(), 3);
    }

    #[test]
    fn spilled_tool_groups_render_one_row_like_unspilled_calls() {
        for (description, code) in [(Some("状態を確認"), 0), (None, 7), (Some("失敗確認"), 7)]
        {
            let mut args = serde_json::json!({"command":"git status"});
            if let Some(description) = description {
                args["description"] = description.into();
            }
            let spill = serde_json::json!({"output":[
                {"type":"function_call","name":"shell","call_id":"c","arguments":args.to_string()},
                {"type":"function_call_output","call_id":"c","output":serde_json::json!({"omitted":true,"call_id":"c","status":"exited","exit_code":code,"execution_id":"e","original_node_id":"r"}).to_string()}
            ],"spill_version":1,"source_node_ids":["a","r"]});
            let text = crate::history::text_of_payload(&spill);
            let mut node = UiNode {
                node_id: "spill".into(),
                role: "spill".into(),
                text: text.clone(),
                total_chars: text.chars().count(),
                presentation: crate::presentation::project("spill", &spill, &text, 0),
                ..Default::default()
            };
            for block in &mut node.presentation.blocks {
                if let Some(tool) = &mut block.tool {
                    tool.group_id = Some("g".into());
                    tool.call_node_id = Some("spill".into());
                    tool.result_node_id = Some("spill".into());
                    tool.ordinal = Some(0);
                    tool.description = description.map(str::to_owned);
                    tool.command = "git status".into();
                    tool.elapsed_ms = Some(400);
                    tool.current_state = Some("exited".into());
                    tool.current_exit_code = Some(code);
                    tool.execution_id = Some("e".into());
                    if block.kind == BlockKind::ToolCall {
                        tool.status = "completed".into();
                    }
                }
            }
            let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
            view.describe_calls = true;
            *view.nodes_mut_for_test() = vec![node];
            let shown = plain(&history(&view, 120).0);
            let rows = shown
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>();
            // Failures open by default, exactly as for unspilled calls.
            if code == 0 {
                assert_eq!(rows.len(), 1, "{shown}");
            }
            assert!(!shown.contains('▹'), "{shown}");
            assert!(
                rows[0].contains(description.unwrap_or("git status")),
                "{shown}"
            );
            assert!(
                rows[0].contains(&format!("exit {code}")) && rows[0].contains("0.4秒"),
                "{shown}"
            );
        }
    }

    #[test]
    fn interruption_details_group_only_with_their_explicit_saved_summary() {
        fn node(id: &str, kind: &str, value: serde_json::Value) -> UiNode {
            let payload = serde_json::json!({"text":value.to_string()});
            let text = crate::history::text_of_payload(&payload);
            UiNode {
                node_id: id.into(),
                text: text.clone(),
                total_chars: text.chars().count(),
                presentation: crate::presentation::project(kind, &payload, &text, 0),
                ..Default::default()
            }
        }
        let detail = node(
            "detail",
            "interruption_details",
            serde_json::json!({"section":"runs","items":[{"run_id":"r"}]}),
        );
        let summary = node(
            "summary",
            "interruption_context",
            serde_json::json!({"kind":"previous_instance_interrupted","change":{"runs":{"count":1,"detail_node_ids":["detail"]},"result_uncertain":true,"automatic_replay":false}}),
        );
        assert_eq!(
            interruption_group(&[detail.clone(), summary.clone()]),
            Some(1)
        );
        assert_eq!(interruption_group(std::slice::from_ref(&detail)), None);
        let mut unrelated = detail.clone();
        unrelated.node_id = "other-detail".into();
        assert_eq!(interruption_group(&[unrelated, summary.clone()]), None);
        let mut wrong_kind = summary.clone();
        wrong_kind.presentation.kind = PresentationKind::ToolResult;
        assert_eq!(interruption_group(&[detail.clone(), wrong_kind]), None);
        let mut wrong_path = summary.clone();
        for field in &mut wrong_path.presentation.fields {
            if field.key.contains("/detail_node_ids/") {
                field.key = "/arbitrary/detail_node_ids/0".into();
            }
        }
        assert_eq!(interruption_group(&[detail.clone(), wrong_path]), None);
        let mut wrong_section = summary.clone();
        for field in &mut wrong_section.presentation.fields {
            if field.key.contains("/detail_node_ids/") {
                field.key = "/change/shells/detail_node_ids/0".into();
            }
        }
        assert_eq!(interruption_group(&[detail.clone(), wrong_section]), None);
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![detail, summary];
        let (lines, _, sources) = history(&view, 100);
        let shown = plain(&lines);
        assert!(shown.contains("前回の中断記録"));
        assert!(!shown.contains("前回の保存記録"));
        assert!(shown.contains("原文2件"));
        assert!(
            sources
                .iter()
                .any(|source| source.node == 0 && source.covered_end == 2)
        );
        assert_eq!(
            view.nodes().len(),
            2,
            "both source nodes remain available to F6"
        );
    }

    #[test]
    fn adjacent_tool_groups_keep_both_source_anchors_and_original_url_targets() {
        use crate::presentation::{NodePresentation, PresentationBlock, ToolPresentation};
        let url = "https://example.invalid/tool";
        let tool = ToolPresentation {
            group_id: Some("fixture-group".into()),
            call_node_id: Some("call-node".into()),
            result_node_id: Some("result-node".into()),
            call_id: "call".into(),
            command: format!("echo {url}"),
            status: "completed".into(),
            execution_id: Some("execution".into()),
            ..Default::default()
        };
        let call = UiNode {
            node_id: "call-node".into(),
            role: "assistant".into(),
            text: "source call".into(),
            total_chars: 11,
            presentation: NodePresentation {
                kind: PresentationKind::Assistant,
                links: vec![crate::presentation::PresentationLink {
                    label: url.into(),
                    url: url.into(),
                    source_start: 0,
                    source_end: 11,
                }],
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(tool.clone()),
                    text: "source call".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = UiNode {
            node_id: "result-node".into(),
            text: "output".into(),
            total_chars: 6,
            presentation: NodePresentation {
                kind: PresentationKind::ToolResult,
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    tool: Some(tool),
                    text: "output".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![call.clone(), result.clone()];
        let (lines, targets, sources) = history(&view, 100);
        assert_eq!(plain(&lines).matches(url).count(), 1);
        assert_eq!(sources[0].node, 0, "compact header keeps call anchor");
        assert_eq!(sources.last().unwrap().covered_end, 2);
        let item = sources[0].item.as_ref().unwrap();
        assert_eq!(item.source_node_ids, ["call-node", "result-node"]);
        assert_eq!(
            item.primary_node_id, "result-node",
            "F6/original targets the saved result"
        );
        assert_eq!(
            targets[0].as_ref(),
            url,
            "the grouped command retains its call's link binding"
        );
        assert!(
            link_regions(&lines, &targets, 100, lines.len() as u16, 0)
                .iter()
                .any(|region| region.url.as_ref() == url)
        );
        assert_eq!(
            view.nodes().len(),
            2,
            "F6 retains independent call/result sources"
        );
        let mut mixed = call.clone();
        mixed.presentation.blocks.insert(
            0,
            PresentationBlock {
                kind: BlockKind::Plain,
                text: "prose before call".into(),
                ..Default::default()
            },
        );
        *view.nodes_mut_for_test() = vec![mixed, result.clone()];
        assert_eq!(
            plain(&history(&view, 100).0).matches(url).count(),
            1,
            "mixed prose preserves one stable tool row"
        );
        assert!(plain(&history(&view, 100).0).contains("prose before call"));
        *view.nodes_mut_for_test() = vec![
            call.clone(),
            UiNode {
                node_id: "separate".into(),
                role: "user".into(),
                text: "other message".into(),
                total_chars: 13,
                ..Default::default()
            },
            result.clone(),
        ];
        assert_eq!(
            plain(&history(&view, 100).0).matches(url).count(),
            1,
            "nonadjacent saved identity has one header"
        );
        assert!(
            plain(&history(&view, 100).0).contains("other message"),
            "the intervening user input is retained"
        );
        let mut metadata = result;
        metadata.text.clear();
        *view.nodes_mut_for_test() = vec![call, metadata];
        assert_eq!(
            plain(&history(&view, 100).0).matches(url).count(),
            1,
            "metadata-only results cannot replace a useful call"
        );
    }

    #[test]
    fn repeated_plain_urls_are_all_clickable_without_rebinding_semantic_links() {
        let url = "https://example.invalid/same";
        let mut lines = vec![Line::from(format!("日本語 {url} then {url}"))];
        let links = vec![crate::presentation::PresentationLink {
            label: url.into(),
            url: url.into(),
            source_start: 0,
            source_end: 100,
        }];
        let mut targets = Vec::new();
        annotate_links(&mut lines, &links, &mut targets);
        assert_eq!(targets.len(), 1);
        assert_eq!(
            lines[0]
                .spans
                .iter()
                .filter(|span| span.style.underline_color.is_some())
                .count(),
            2
        );
        let regions = link_regions(&lines, &targets, 100, 1, 0);
        assert_eq!(regions.len(), 2);
        assert!(regions.iter().all(|region| region.url.as_ref() == url));
        let original = lines[0].spans.clone();
        annotate_links(&mut lines, &links, &mut targets);
        assert_eq!(
            lines[0].spans, original,
            "an existing semantic binding is not inferred again"
        );
    }

    #[test]
    fn body_and_metadata_notices_report_only_their_known_extent() {
        let text = "完全な本文".repeat(300);
        let payload =
            serde_json::json!({"role":"assistant","content":[{"type":"output_text","text":text}]});
        let mut node = UiNode {
            node_id: "notice".into(),
            role: "assistant".into(),
            text: text.clone(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: crate::presentation::project("assistant_response", &payload, &text, 0),
        };
        assert!(node.presentation.metadata_limited);
        assert_eq!(node.presentation.body_omitted_chars, 0);
        let shown = plain(&node_lines(
            UiLanguage::Ja,
            &node,
            false,
            0,
            80,
            &mut Vec::new(),
        ));
        assert!(!shown.contains("詳細情報は"));
        assert!(omission_notices(UiLanguage::Ja, &node).is_empty());
        assert!(
            !shown.contains("表示用抜粋")
                && !shown.contains("文字省略")
                && !shown.contains("原文の取得範囲")
        );
        assert!(
            node.presentation
                .blocks
                .iter()
                .any(|block| block.text == text)
        );
        node.base_char_offset = 50;
        node.text = "あいうえお".into();
        node.total_chars = 100;
        let notices = omission_notices(UiLanguage::Ja, &node).join("\n");
        assert!(notices.contains("原文の取得範囲 51-55/100文字"));
        node.presentation.body_omitted_chars = 17;
        node.presentation.body_extent_unknown = true;
        let notices = omission_notices(UiLanguage::Ja, &node);
        assert_eq!(notices.len(), 1);
        let notices = notices.join("\n");
        assert!(notices.contains("17文字省略"));
        assert!(notices.contains("省略量は未確認"));
        assert_eq!(node.presentation.fields[0].key, "/content");
    }
    #[test]
    fn unknown_saved_kind_remains_visible_despite_legacy_assistant_role() {
        let node = UiNode {
            node_id: "unknown".into(),
            role: "assistant".into(),
            text: String::new(),
            base_char_offset: 0,
            total_chars: 0,
            reasoning_ranges: vec![],
            presentation: crate::presentation::project(
                "future_node",
                &serde_json::json!({"state":"new"}),
                "",
                0,
            ),
        };
        let shown = plain(&node_lines(
            UiLanguage::Ja,
            &node,
            false,
            0,
            40,
            &mut Vec::new(),
        ));
        assert!(shown.contains("future_node"));
        assert!(
            !shown.contains('▸'),
            "unknown items must not become empty assistant replies"
        );
    }
    #[test]
    fn wrapped_tool_urls_keep_the_complete_target_on_every_visible_fragment() {
        for repeats in [5, 30] {
            let url = format!("https://example.test/{}", "abcdefghij".repeat(repeats));
            for kind in [BlockKind::ToolCall, BlockKind::ToolResult] {
                let node = UiNode {
                    node_id: "wrapped-url".into(),
                    role: "tool".into(),
                    text: url.clone(),
                    base_char_offset: 0,
                    total_chars: url.len(),
                    reasoning_ranges: vec![],
                    presentation: crate::presentation::NodePresentation {
                        kind: if kind == BlockKind::ToolCall {
                            PresentationKind::ToolCall
                        } else {
                            PresentationKind::ToolResult
                        },
                        blocks: vec![crate::presentation::PresentationBlock {
                            kind: kind.clone(),
                            text: url.clone(),
                            source_end: url.len(),
                            ..Default::default()
                        }],
                        links: vec![crate::presentation::PresentationLink {
                            label: url.clone(),
                            url: url.clone(),
                            source_end: url.len(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                };
                let mut targets = Vec::new();
                let lines = node_lines(UiLanguage::Ja, &node, false, 0, 24, &mut targets);
                let regions = link_regions(&lines, &targets, 24, 40, 0);
                assert!(regions.len() > 1, "{kind:?}: {}", plain(&lines));
                assert!(regions.iter().all(|region| region.url.as_ref() == url));
                let linked = regions.iter().map(|r| usize::from(r.width)).sum::<usize>();
                if kind == BlockKind::ToolResult && repeats == 30 {
                    assert!(linked < url.len(), "folded rows have no hit cells");
                    assert!(plain(&lines).contains("出力抜粋"));
                } else {
                    assert_eq!(linked, url.len(), "{kind:?}");
                }
                if kind == BlockKind::ToolResult {
                    assert!(
                        regions.iter().all(|region| region.x >= 4),
                        "indent guides are not links"
                    );
                }
                if kind == BlockKind::ToolCall && repeats == 5 {
                    let mut typed = node.clone();
                    typed.presentation.blocks[0].tool =
                        Some(crate::presentation::ToolPresentation {
                            call_id: "call-url".into(),
                            ordinal: Some(0),
                            command: url.clone(),
                            status: "requested".into(),
                            ..Default::default()
                        });
                    let mut targets = Vec::new();
                    let lines = node_lines(UiLanguage::Ja, &typed, false, 0, 24, &mut targets);
                    let regions = link_regions(&lines, &targets, 24, 40, 0);
                    assert!(regions.len() > 1);
                    assert!(regions.iter().all(|region| region.url.as_ref() == url));
                    assert_eq!(
                        regions.iter().map(|r| usize::from(r.width)).sum::<usize>(),
                        url.len()
                    );
                }
            }
        }
    }
    fn markdown_fixture(
        text: &str,
        width: u16,
    ) -> (UiNode, Vec<Line<'static>>, Vec<std::sync::Arc<str>>) {
        let mut projected = crate::presentation::project(
            "assistant",
            &serde_json::json!({"role":"assistant","content":[{"type":"output_text","text":text}]}),
            text,
            0,
        );
        crate::presentation::render_markdown(&mut projected, width - 2);
        let node = UiNode {
            node_id: "semantic-links".into(),
            role: "assistant".into(),
            text: text.into(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: projected,
        };
        let mut targets = Vec::new();
        let lines = node_lines(UiLanguage::Ja, &node, false, 0, width, &mut targets);
        (node, lines, targets)
    }
    fn marked_text(lines: &[Line<'static>], targets: &[std::sync::Arc<str>], url: &str) -> String {
        let index = targets
            .iter()
            .position(|target| target.as_ref() == url)
            .unwrap();
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| {
                span.style.underline_color == Some(crate::terminal_links::marker_color(index))
            })
            .map(|span| span.content.as_ref())
            .collect()
    }
    #[test]
    fn markdown_click_identity_survives_duplicate_decorated_and_url_labels() {
        let text = "[same](https://one.test/a) [same](https://two.test/b)\n\n[**bold** *italic* `code`](https://three.test/c) [https://one.test/a](https://four.test/d) [https://one.test/a `https://one.test/a`](javascript:bad) `https://code.test/a`";
        let (node, lines, targets) = markdown_fixture(text, 28);
        assert_eq!(node.text, text);
        for (url, label) in [
            ("https://one.test/a", "same"),
            ("https://two.test/b", "same"),
            ("https://three.test/c", "bold italic code"),
            ("https://four.test/d", "https://one.test/a"),
        ] {
            let marked = marked_text(&lines, &targets, url);
            assert!(marked.starts_with(label), "{url}: {marked:?}");
            assert!(
                marked.ends_with(url),
                "OSS destination also retains the same identity: {marked:?}"
            );
        }
        assert_eq!(
            marked_text(&lines, &targets, "https://one.test/a"),
            "samehttps://one.test/a"
        );
        assert_eq!(
            marked_text(&lines, &targets, "https://code.test/a"),
            "https://code.test/a"
        );
        let regions = link_regions(&lines, &targets, 28, 100, 0);
        for target in &targets {
            assert!(regions.iter().any(|region| &region.url == target));
            assert!(
                node.presentation
                    .links
                    .iter()
                    .any(|link| link.url == target.as_ref())
            );
        }
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("bold")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
    }
    #[test]
    fn markdown_link_callbacks_cover_tables_references_images_and_unavailable_targets() {
        let text = "| name | target |\n| --- | --- |\n| [same][first] | [**same**](https://table.test/b) |\n\n[first]: https://ref.test/a\n\n[![image [inner](https://inner.test/a)](image.png)](https://outer.test/a)\n\n[bad](javascript:bad) [following](https://following.test/a)\n\nFootnote[^1]\n\n[^1]: [foot](https://foot.test/a)";
        let (_, lines, targets) = markdown_fixture(text, 40);
        for (url, label) in [
            ("https://ref.test/a", "same"),
            ("https://table.test/b", "same"),
            ("https://following.test/a", "following"),
            ("https://foot.test/a", "foot"),
        ] {
            assert!(
                marked_text(&lines, &targets, url).contains(label),
                "{url}: {}",
                plain(&lines)
            );
        }
        assert!(!targets.iter().any(|url| url.starts_with("javascript:")));
        // A rejected destination must not shift the next valid callback's identity.
        assert_eq!(
            marked_text(&lines, &targets, "https://following.test/a"),
            "followinghttps://following.test/a"
        );
        assert!(!marked_text(&lines, &targets, "https://following.test/a").contains("inner"));
    }
    #[test]
    fn markdown_long_label_regions_follow_wrap_and_source_metadata_remains_canonical() {
        let label = "長いラベル".repeat(12);
        let text = format!("[**{label}**](https://long.test/a)\n\n[same](https://later.test/b)");
        let (node, lines, targets) = markdown_fixture(&text, 22);
        let regions = link_regions(&lines, &targets, 22, 100, 0);
        let long = regions
            .iter()
            .filter(|region| region.url.as_ref() == "https://long.test/a")
            .collect::<Vec<_>>();
        assert!(long.len() > 2);
        assert!(long.iter().all(|region| region.x + region.width <= 22));
        assert_eq!(
            marked_text(&lines, &targets, "https://long.test/a"),
            format!("{label}https://long.test/a")
        );
        assert_eq!(node.text, text);
        assert!(
            node.presentation
                .links
                .iter()
                .any(|link| link.url == "https://long.test/a" && link.label.contains(&label))
        );
    }
    #[test]
    fn oss_markdown_keeps_link_cells_and_inline_hanging_markers() {
        let text = "# 見出し\n\n日本語の長い文章です。[完全な参照](https://example.test/a/b?long=yes)\n\n```rust\nlet x = 1;\n```";
        let mut projected = crate::presentation::project(
            "assistant",
            &serde_json::json!({"role":"assistant","content":[{"type":"output_text","text":text}]}),
            text,
            0,
        );
        crate::presentation::render_markdown(&mut projected, 18);
        let node = UiNode {
            node_id: "node".into(),
            role: "assistant".into(),
            text: text.into(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: projected,
        };
        let mut targets = Vec::new();
        // Viewport clipping slices whole wrapped rows and preserves line styles.
        let lines = node_lines(UiLanguage::Ja, &node, false, 0, 20, &mut targets);
        assert!(plain(&lines).starts_with("▸ "));
        assert!(
            lines
                .iter()
                .skip(1)
                .filter(|line| !line.spans.is_empty())
                .all(|line| line.spans[0].content == "  ")
        );
        assert_eq!(targets[0].as_ref(), "https://example.test/a/b?long=yes");
        let count = ratatui::widgets::Paragraph::new(borrowed(&lines))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .line_count(20);
        let regions = link_regions(&lines, &targets, 20, 10, count.saturating_sub(10) as u16);
        assert!(!regions.is_empty());
        assert!(
            regions
                .iter()
                .all(|region| region.x + region.width <= 20 && region.y < 10)
        );
        assert!(
            borrowed(&lines)
                .iter()
                .flat_map(|l| &l.spans)
                .all(|span| span.style.underline_color.is_none())
        );
        use ratatui::widgets::Widget;
        let area = ratatui::layout::Rect::new(0, 0, 20, count as u16);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Paragraph::new(borrowed(&lines))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .render(area, &mut buffer);
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| cell.bg == Color::Rgb(30, 34, 40)),
            "OSS code-block background must survive the UI layout and borrowed draw path"
        );
    }
    #[test]
    fn code_surface_keeps_readable_foreground_on_dark_and_light_terminal_styles() {
        use ratatui::widgets::{Paragraph, Widget};
        for language in ["text", "unrecognized_language", "rust"] {
            let text = format!("```{language}\nlet sample = 17;\n``` ");
            let mut projected = crate::presentation::project(
                "assistant",
                &serde_json::json!({"role":"assistant","content":[{"type":"output_text","text":text}]}),
                &text,
                0,
            );
            crate::presentation::render_markdown(&mut projected, 48);
            let node = UiNode {
                node_id: "code-pair".into(),
                role: "assistant".into(),
                text: text.clone(),
                base_char_offset: 0,
                total_chars: text.chars().count(),
                reasoning_ranges: vec![],
                presentation: projected,
            };
            let lines = node_lines(UiLanguage::Ja, &node, false, 0, 50, &mut Vec::new());
            for terminal_foreground in [Color::Black, Color::White] {
                let area = ratatui::layout::Rect::new(0, 0, 50, 10);
                let mut buffer = ratatui::buffer::Buffer::empty(area);
                Paragraph::new(borrowed(&lines))
                    .style(Style::default().fg(terminal_foreground))
                    .render(area, &mut buffer);
                let code_cells: Vec<_> = buffer
                    .content
                    .iter()
                    .filter(|cell| cell.bg == Color::Rgb(30, 34, 40) && cell.symbol() != " ")
                    .collect();
                assert!(!code_cells.is_empty(), "{language}");
                assert!(
                    code_cells
                        .iter()
                        .all(|cell| !matches!(cell.fg, Color::Reset | Color::Black | Color::White)),
                    "{language}: code must not inherit the terminal foreground"
                );
                if language == "rust" {
                    assert!(
                        code_cells.iter().any(|cell| !matches!(
                            cell.fg,
                            Color::Rgb(208, 214, 223) | Color::Rgb(137, 151, 169)
                        )),
                        "OSS syntax colors must remain"
                    );
                } else {
                    let mut body_node = node.clone();
                    let mut captions_removed = 0;
                    for block in &mut body_node.presentation.blocks {
                        if let Some(rendered) = &mut block.rendered {
                            let previous = rendered.lines.len();
                            rendered.lines.retain(|line| {
                                !line.spans.iter().any(|span| {
                                    span.style.underline_color == Some(theme::COPY_DECORATION)
                                })
                            });
                            captions_removed += previous - rendered.lines.len();
                        }
                    }
                    assert!(captions_removed > 0, "generated caption must be excluded");
                    let body_lines =
                        node_lines(UiLanguage::Ja, &body_node, false, 0, 50, &mut Vec::new());
                    let mut body_buffer = ratatui::buffer::Buffer::empty(area);
                    Paragraph::new(borrowed(&body_lines))
                        .style(Style::default().fg(terminal_foreground))
                        .render(area, &mut body_buffer);
                    let samples: Vec<_> = body_buffer
                        .content
                        .iter()
                        .filter(|cell| cell.bg == Color::Rgb(30, 34, 40) && cell.symbol() == "s")
                        .collect();
                    assert!(!samples.is_empty(), "plain code sample must be rendered");
                    assert!(
                        samples
                            .iter()
                            .all(|cell| cell.fg == Color::Rgb(208, 214, 223)),
                        "plain code uses its paired foreground"
                    );
                }
            }
        }
    }
    #[test]
    fn semantic_markers_belong_to_answer_reasoning_events_and_summary_without_text_detection() {
        let payload = serde_json::json!({"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"検討の本文"}]},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"回答の本文"}]}]});
        let text = crate::history::text_of_payload(&payload);
        let node = UiNode {
            node_id: "mixed".into(),
            role: "assistant".into(),
            text: text.clone(),
            base_char_offset: 0,
            total_chars: text.chars().count(),
            reasoning_ranges: vec![],
            presentation: crate::presentation::project("assistant_response", &payload, &text, 0),
        };
        for expanded in [false, true] {
            let shown = node_lines(UiLanguage::Ja, &node, expanded, 0, 80, &mut Vec::new());
            let shown = plain(&shown);
            assert!(
                shown
                    .lines()
                    .any(|line| line.trim_start().starts_with("▸ 推論"))
            );
            assert!(shown.lines().any(|line| line.starts_with("▸ 回答の本文")));
            assert!(!shown.lines().any(|line| line.starts_with("▸ 推論")));
        }
        let payload = serde_json::json!({"text":"要約の本文","source_node_ids":["one","two"]});
        let text = crate::history::text_of_payload(&payload);
        let node = UiNode {
            presentation: crate::presentation::project("summary", &payload, &text, 0),
            text,
            total_chars: 6,
            role: "summary".into(),
            ..node
        };
        assert!(
            plain(&node_lines(
                UiLanguage::Ja,
                &node,
                false,
                0,
                80,
                &mut Vec::new()
            ))
            .starts_with("⬦ 要約 | 出典2件")
        );
    }
    #[test]
    fn tool_calls_use_inline_shell_marker_without_assistant_role_line() {
        let command = "printf synthetic-marker";
        let node = UiNode {
            node_id: "call".into(),
            role: "assistant".into(),
            text: command.into(),
            base_char_offset: 0,
            total_chars: command.len(),
            reasoning_ranges: vec![],
            presentation: crate::presentation::NodePresentation {
                kind: PresentationKind::Assistant,
                title: "Shellboy".into(),
                blocks: vec![crate::presentation::PresentationBlock {
                    kind: BlockKind::ToolCall,
                    text: command.into(),
                    source_start: 0,
                    source_end: command.len(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        };
        let shown = node_lines(UiLanguage::Ja, &node, false, 0, 80, &mut Vec::new());
        assert_eq!(plain(&shown), "▹ shell  printf synthetic-marker");
        assert!(
            shown[0]
                .spans
                .iter()
                .any(|span| span.content.contains("printf") && span.style.fg == Some(Color::Cyan))
        );
    }
    #[test]
    fn failed_tool_output_is_indented_head_tail_excerpt_with_full_original_retained() {
        let output = (0..40)
            .map(|index| format!("出力 {index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let node = UiNode {
            node_id: "result".into(),
            role: "tool".into(),
            text: output.clone(),
            base_char_offset: 0,
            total_chars: output.chars().count(),
            reasoning_ranges: vec![],
            presentation: crate::presentation::NodePresentation {
                kind: PresentationKind::ToolResult,
                title: "shell | exited | 失敗 exit 101".into(),
                fields: vec![crate::presentation::PresentationField {
                    value_omitted: false,
                    display: None,
                    key: "/exit_code".into(),
                    label: "exit".into(),
                    value: "101".into(),
                }],
                blocks: vec![crate::presentation::PresentationBlock {
                    kind: BlockKind::ToolResult,
                    text: output.clone(),
                    source_start: 0,
                    source_end: output.chars().count(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        };
        for width in [35, 80] {
            let shown = node_lines(UiLanguage::Ja, &node, false, 0, width, &mut Vec::new());
            assert!(plain(&shown).contains("出力 00") && plain(&shown).contains("出力 39"));
            assert!(!plain(&shown).contains("出力 20"));
            assert!(plain(&shown).contains("抜粋") && plain(&shown).contains("F6 詳細"));
            assert!(!plain(&shown[..1]).contains("stdout"));
            assert_eq!(shown[0].spans[0].style.fg, Some(Color::LightRed));
            assert!(plain(&shown[1..]).starts_with("  | 出力 00"));
            assert!(shown.len() < 10);
        }
        assert_eq!(node.text, output);
        assert_eq!(node.presentation.blocks[0].text, output);
    }
    #[test]
    fn typed_output_blocks_use_guides_not_stream_titles_and_combined_is_not_stderr() {
        let original = "保存済み原文";
        let mut blocks = Vec::new();
        for (stream, text) in [
            (OutputStream::Stdout, "標準出力の本文".to_owned()),
            (
                OutputStream::Stderr,
                "失敗の詳しい説明と日本語の長い行。".repeat(12),
            ),
            (
                OutputStream::Combined,
                "errorという文字を含む結合出力".to_owned(),
            ),
        ] {
            blocks.push(crate::presentation::PresentationBlock {
                kind: BlockKind::ToolResult,
                text,
                stream: Some(stream),
                ..Default::default()
            });
        }
        let node = UiNode {
            node_id: "result".into(),
            role: "tool".into(),
            text: original.into(),
            base_char_offset: 0,
            total_chars: original.chars().count(),
            reasoning_ranges: vec![],
            presentation: crate::presentation::NodePresentation {
                kind: PresentationKind::ToolResult,
                title: "shell | exited".into(),
                blocks,
                ..Default::default()
            },
        };
        let lines = node_lines(UiLanguage::Ja, &node, false, 0, 35, &mut Vec::new());
        let text = plain(&lines);
        assert!(!text.contains("stdout") && !text.contains("stderr") && !text.contains("Combined"));
        assert!(lines.iter().all(|line| line.width() <= 35));
        let colors = lines
            .iter()
            .filter_map(|line| {
                line.spans
                    .first()
                    .filter(|span| span.content == "  | ")
                    .and_then(|span| span.style.fg)
            })
            .collect::<Vec<_>>();
        assert_eq!(colors[0], Color::Reset);
        assert!(colors.contains(&Color::LightRed));
        assert_eq!(colors.last(), Some(&Color::Reset));
        assert!(text.contains("抜粋") && text.contains("F6 詳細"));
        assert_eq!(lines.iter().filter(|line| line.spans.is_empty()).count(), 2);
        assert_eq!(node.text, original);
    }

    #[test]
    fn call_and_result_exit_markers_match_their_attention_color() {
        for (code, mark, color) in [
            (Some(0), "✓", Color::Cyan),
            (Some(7), "✗", Color::LightRed),
            (None, "◌", Color::Cyan),
        ] {
            for kind in ["assistant_response", "tool_result"] {
                let payload = if kind == "assistant_response" {
                    serde_json::json!({"output":[{"type":"function_call","name":"shell","call_id":"c","arguments":"{\"command\":\"printf fixture\"}"}]})
                } else {
                    serde_json::json!({"item":{"type":"function_call_output","call_id":"c","output":serde_json::json!({"status":"exited","exit_code":code,"command":"printf fixture","output":"fixture"}).to_string()}})
                };
                let text = crate::history::text_of_payload(&payload);
                let mut node = UiNode {
                    node_id: kind.into(),
                    role: kind.into(),
                    text: text.clone(),
                    base_char_offset: 0,
                    total_chars: text.chars().count(),
                    reasoning_ranges: vec![],
                    presentation: crate::presentation::project(kind, &payload, &text, 0),
                };
                let tool = node.presentation.blocks[0].tool.as_mut().unwrap();
                tool.ordinal = Some(1);
                tool.current_state = Some("exited".into());
                tool.current_exit_code = code;
                let lines = node_lines(UiLanguage::Ja, &node, false, 0, 160, &mut Vec::new());
                let header = lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .find(|span| span.content.contains("#2 printf fixture"))
                    .unwrap();
                assert!(
                    header.content.starts_with(&format!(
                        "{} #2 printf fixture",
                        if kind == "assistant_response" {
                            "▹"
                        } else {
                            mark
                        }
                    )),
                    "{}",
                    header.content
                );
                assert_eq!(header.style.fg, Some(color));
                if kind == "assistant_response" {
                    assert!(
                        !header.content.contains("終了"),
                        "terminal facts belong to the result heading"
                    );
                }
            }
        }
    }
    #[test]
    fn plain_tabs_expand_before_word_wrapping_and_preserve_styles() {
        let style = Style::default().fg(theme::ATTENTION);
        let lines = hanging_wrap(
            vec![Line::from(vec![
                Span::raw("日"),
                Span::styled("\tX", style),
            ])],
            5,
        );
        let text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(text, ["日  X"]);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.contains('X') && span.style.fg == style.fg)
        );
    }
}

#[cfg(test)]
mod conversation_item_tests {

    #[test]
    fn boundary_rows_are_unselectable_and_tools_stay_dense() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        let call = |group: &str| PresentationBlock {
            kind: BlockKind::ToolCall,
            tool: Some(tool(group, "completed")),
            ..Default::default()
        };
        *view.nodes_mut_for_test() = vec![
            node("before", "before", vec![prose("before", 0)]),
            node("calls", "", vec![call("a"), call("b")]),
            node("after", "after", vec![prose("after", 0)]),
        ];
        let original = view
            .nodes()
            .iter()
            .map(|n| n.text.clone())
            .collect::<Vec<_>>();
        let (lines, _, sources) = history(&view, 80);
        let tools = sources
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                matches!(s.item.as_ref().map(|r| &r.key), Some(ItemKey::Tool { .. })).then_some(i)
            })
            .collect::<Vec<_>>();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1], tools[0] + 1);
        for (line, source) in lines.iter().zip(&sources) {
            if line.spans.is_empty() {
                assert!(
                    source.item.is_none(),
                    "generated blank must not be selectable"
                );
            }
        }
        assert_eq!(
            view.nodes()
                .iter()
                .map(|n| n.text.clone())
                .collect::<Vec<_>>(),
            original
        );
    }
    #[test]
    fn equivalent_terminal_metadata_does_not_reset_a_closed_disclosure() {
        let mut saved = tool("one", "failed");
        saved.exit_code = Some(1);
        let mut observed = saved.clone();
        observed.current_state = Some("exited".into());
        observed.current_exit_code = Some(1);
        assert_eq!(tool_outcome(&saved), tool_outcome(&observed));
        let mut success = tool("one", "completed");
        let before = tool_outcome(&success);
        success.current_state = Some("exited".into());
        success.current_exit_code = Some(0);
        assert_eq!(before, tool_outcome(&success));
        assert_ne!(tool_outcome(&saved), tool_outcome(&success));
    }
    #[test]
    fn duplicate_result_outcomes_cannot_reopen_a_closed_canonical_header() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        let call = tool("shared", "running");
        let mut old = tool("shared", "failed");
        old.exit_code = Some(7);
        let latest = tool("shared", "completed");
        let key = ItemKey::tool(&latest, "latest", 0);
        let outcome = tool_outcome(&latest);
        assert!(view.items.toggle(key.clone(), false, &outcome));
        assert!(!view.items.toggle(key.clone(), false, &outcome));
        // The old duplicate-body predicate used this stale result's own default.
        assert!(
            view.items
                .is_open(&key, old.requires_attention(), &tool_outcome(&old))
        );
        *view.nodes_mut_for_test() = vec![
            node(
                "call",
                "",
                vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(call),
                    ..Default::default()
                }],
            ),
            node("middle", "KEEP_PROSE", vec![prose("KEEP_PROSE", 0)]),
            node(
                "old",
                "STALE_RESULT_ERROR",
                vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    source_exact: true,
                    text: "STALE_RESULT_ERROR".into(),
                    tool: Some(old),
                    ..Default::default()
                }],
            ),
            node(
                "latest",
                "LATEST_RESULT",
                vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    source_exact: true,
                    text: "LATEST_RESULT".into(),
                    tool: Some(latest),
                    ..Default::default()
                }],
            ),
        ];
        let shown = plain(&history(&view, 80).0);
        assert!(shown.contains("KEEP_PROSE"));
        assert_eq!(shown.matches("printf synthetic").count(), 1);
        assert!(
            !shown.contains("STALE_RESULT_ERROR") && !shown.contains("LATEST_RESULT"),
            "{shown}"
        );
    }
    #[test]
    fn missing_group_result_uses_its_exact_owner_and_keeps_richer_observed_header() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        let mut call = tool("owner", "completed");
        call.result_node_id = Some("legacy".into());
        let key = ItemKey::tool(&call, "call", 0);
        let outcome = tool_outcome(&call);
        assert!(view.items.toggle(key.clone(), false, &outcome));
        assert!(!view.items.toggle(key, false, &outcome));
        let mut legacy = tool("owner", "failed");
        legacy.group_id = None;
        legacy.exit_code = Some(7);
        *view.nodes_mut_for_test() = vec![
            node(
                "call",
                "",
                vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(call),
                    ..Default::default()
                }],
            ),
            node(
                "legacy",
                "STALE_LEGACY",
                vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    tool: Some(legacy),
                    source_exact: true,
                    text: "STALE_LEGACY".into(),
                    ..Default::default()
                }],
            ),
        ];
        let shown = plain(&history(&view, 80).0);
        assert_eq!(shown.matches("printf synthetic").count(), 1);
        assert!(shown.contains("✓ printf synthetic"), "{shown}");
        assert!(!shown.contains("STALE_LEGACY"), "{shown}");
    }
    #[test]
    fn empty_exact_result_members_do_not_leave_separate_detail_placeholders() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        let mut call = tool("shared", "completed");
        call.result_node_id = Some("empty-result".into());
        let mut result = node("empty-result", "", vec![]);
        result.role = "tool".into();
        result.presentation.kind = PresentationKind::ToolResult;
        *view.nodes_mut_for_test() = vec![
            node(
                "call",
                "",
                vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(call),
                    ..Default::default()
                }],
            ),
            result,
        ];
        let (lines, _, sources) = history(&view, 80);
        let shown = plain(&lines);
        assert_eq!(shown.matches("printf synthetic").count(), 1);
        assert!(!shown.contains("詳細で内容・参照を確認"), "{shown}");
        assert!(
            sources
                .iter()
                .all(|s| !matches!(s.item.as_ref().map(|i| &i.key), Some(ItemKey::Node(_))))
        );
    }
    #[test]
    fn description_replaces_command_until_existing_focus_or_disclosure_reveals_it() {
        let config = japanese_config();
        let mut view = ThreadView::new(&config, std::env::temp_dir());
        let mut value = tool("described", "completed");
        value.description = Some("Inspect repository".into());
        let key = ItemKey::tool(&value, "call", 0);
        let outcome = tool_outcome(&value);
        let command = value.command.clone();
        *view.nodes_mut_for_test() = vec![node(
            "call",
            "",
            vec![PresentationBlock {
                kind: BlockKind::ToolCall,
                tool: Some(value),
                ..Default::default()
            }],
        )];
        let collapsed = plain(&history(&view, 80).0);
        assert!(collapsed.contains("Inspect repository"));
        assert!(!collapsed.contains(&command), "{collapsed}");
        view.items.active = true;
        view.items.focus = Some(key.clone());
        let focused_collapsed = plain(&history(&view, 80).0);
        assert!(focused_collapsed.contains("Inspect repository"));
        assert!(
            !focused_collapsed.contains(&command),
            "focus must not keep a collapsed command preview open: {focused_collapsed}"
        );
        view.items.toggle(key.clone(), false, &outcome);
        let expanded = plain(&history(&view, 80).0);
        assert!(expanded.contains(&command), "{expanded}");
        view.items.toggle(key.clone(), false, &outcome);
        let collapsed_focused = plain(&history(&view, 80).0);
        assert!(
            !collapsed_focused.contains(&command),
            "closing the row must close its command preview: {collapsed_focused}"
        );
        view.items.toggle(key, false, &outcome);
        view.describe_calls = false;
        let opted_out = plain(&history(&view, 80).0);
        assert!(opted_out.contains(&command));
        assert!(!opted_out.contains("Inspect repository"));
    }
    #[test]
    fn blank_descriptions_fall_back_to_command_even_for_direct_presentations() {
        for description in ["", "  ", "\n\t\u{1b}"] {
            let mut value = tool("blank", "completed");
            value.description = Some(description.into());
            let text = compact_tool_line(
                UiLanguage::Ja,
                &value,
                120,
                theme::Palette::detect(crate::config::PaletteMode::Dark),
            )
            .to_string();
            assert!(text.contains(&value.command), "{text}");
        }
    }
    #[test]
    fn compact_tool_rows_hide_ids_and_only_label_factual_background_work() {
        use crate::presentation::ToolReasonKind;
        let palette = theme::Palette::detect(crate::config::PaletteMode::Dark);
        for status in [
            "running",
            "running_current",
            "completed",
            "exited",
            "cancelled",
        ] {
            for (explicit_background, reason, expected) in [
                (false, ToolReasonKind::Unknown, ""),
                (true, ToolReasonKind::Unknown, "背景"),
                (false, ToolReasonKind::HumanBackgrounded, "背景（利用者）"),
                (false, ToolReasonKind::AdditionalInput, "背景（追加入力）"),
            ] {
                let mut value = tool("display-only", status);
                value.execution_id = Some("3153b821-full-diagnostic-id".into());
                value.background = explicit_background;
                value.reason_kind = reason;
                for width in [80, 140] {
                    let line = compact_tool_line(UiLanguage::Ja, &value, width, palette);
                    let text = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    assert!(!text.contains("3153b821"), "{text}");
                    assert_eq!(
                        text.contains("背景"),
                        !expected.is_empty(),
                        "{status}: {text}"
                    );
                    if !expected.is_empty() {
                        assert!(text.contains(expected), "{text}");
                    }
                    assert!(text.contains("printf synthetic"), "{text}");
                }
                assert_eq!(
                    value.execution_id.as_deref(),
                    Some("3153b821-full-diagnostic-id")
                );
            }
        }
        let mut released = tool("display-only", "running");
        released.current_state = Some("background_current".into());
        let line = compact_tool_line(UiLanguage::Ja, &released, 80, palette);
        assert!(line.spans.last().unwrap().content.contains("背景"));
    }
    #[test]
    fn clipped_tool_text_uses_only_measured_ellipsis() {
        use crate::ui::text_width::UnicodeWidthStr;
        for (observed, ellipsis) in [(Some(1), "…"), (Some(2), "."), (None, ".")] {
            crate::ui::text_width::set_terminal_width(observed);
            assert_eq!(tool_text_fit("abcdef", 4), format!("abc{ellipsis}"));
            assert_eq!(tool_text_fit("abcdef", 0), "");
            assert_eq!(tool_text_fit("ok", 4), "ok");
            for width in 0..10 {
                let value = tool_text_fit("日e\u{301}本語abcdefgh", width);
                assert!(value.width() <= width, "{observed:?} {width}: {value}");
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn compact_tool_metadata_uses_only_measured_ambiguous_width() {
        let mut value = tool("parts", "completed");
        value.background = true;
        value.elapsed_ms = Some(12_400);
        for (observed, separator) in [(Some(1), " · "), (Some(2), " | "), (None, " | ")] {
            crate::ui::text_width::set_terminal_width(observed);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let (_, metadata, _) = tool_metadata(language, &value, None);
                assert_eq!(
                    metadata.matches(separator).count(),
                    2,
                    "{observed:?}: {metadata}"
                );
            }
        }
        crate::ui::text_width::clear_terminal_width();
    }

    #[test]
    fn compact_tool_metadata_never_enters_body_and_yields_to_body_before_attention() {
        let palette = theme::Palette::detect(crate::config::PaletteMode::Dark);
        let mut value = tool("parts", "completed");
        value.background = true;
        value.elapsed_ms = Some(12_400);
        let line = compact_tool_line(UiLanguage::Ja, &value, 80, palette);
        let body = body_spans(&line)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(body, "printf synthetic");
        assert_eq!(line.spans.last().unwrap().content, "背景 | exit 0 | 12.4秒");
        assert_eq!(line.width(), 80);
        value.command = "abcdefghijklmnopqrstuvwxyz0123456789".into();
        let line = compact_tool_line(UiLanguage::Ja, &value, 30, palette);
        let body = body_spans(&line)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(body, tool_text_fit(&value.command, 28));
        assert!(!line.spans.iter().any(|s| s.content.contains("背景")));
        value.status = "failed".into();
        value.exit_code = Some(7);
        let line = compact_tool_line(UiLanguage::Ja, &value, 30, palette);
        assert!(line.spans.last().unwrap().content.contains("exit 7"));
        assert_eq!(
            line.spans.last().unwrap().style.fg,
            Some(RowKind::Failure.metadata_color(palette))
        );
        assert_eq!(line.width(), 30);
    }
    #[test]
    fn uncached_and_details_hints_remain_indented_body_rows() {
        for language in [UiLanguage::Ja, UiLanguage::En] {
            for width in [12, 48] {
                let uncached = UiNode {
                    node_id: "uncached".into(),
                    role: "assistant".into(),
                    total_chars: 200,
                    presentation: crate::presentation::NodePresentation {
                        kind: PresentationKind::Unknown,
                        title: "Hint".into(),
                        body_evicted: true,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let details = UiNode {
                    node_id: "details".into(),
                    role: "developer".into(),
                    text: "synthetic system body".into(),
                    total_chars: 20,
                    presentation: crate::presentation::NodePresentation {
                        kind: PresentationKind::Unknown,
                        title: "Policy".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                for node in [&uncached, &details] {
                    let rows = node_lines(language, node, false, 0, width, &mut Vec::new());
                    assert!(rows.len() >= 2, "{language:?} {width}: {}", plain(&rows));
                    for row in rows.iter().skip(1).filter(|row| row.width() > 0) {
                        let shown = row
                            .spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>();
                        assert!(shown.starts_with("  "), "{language:?} {width}: {shown}");
                        assert!(
                            row.width() <= usize::from(width),
                            "{language:?} {width}: {shown}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn indented_wrap_keeps_nested_rows_and_continuations_aligned_bilingually() {
        for (language, text) in [
            (
                UiLanguage::En,
                "A nested English explanation that wraps across several rows.",
            ),
            (
                UiLanguage::Ja,
                "入れ子の日本語説明文が狭い幅で複数行に折り返されます。",
            ),
        ] {
            for width in [2, 4, 12, 48] {
                let rows = indented_wrap(vec![Line::from(text)], width);
                assert!(
                    rows.len() > 1 || width == 48,
                    "{language:?} {width}: {rows:?}"
                );
                for row in &rows {
                    let shown = row
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    if width >= 3 {
                        assert!(shown.starts_with("  "), "{language:?} {width}: {shown}");
                    }
                    assert!(
                        row.width() <= usize::from(width),
                        "{language:?} {width}: {shown}"
                    );
                }
                if width >= 3 {
                    let muted = indented_wrap(
                        vec![Line::styled(text, Style::default().fg(theme::MUTED))],
                        width,
                    );
                    assert!(muted.iter().all(|line| {
                        line.spans
                            .iter()
                            .all(|span| line.style.patch(span.style).fg == Some(theme::MUTED))
                    }));
                }
            }
        }
    }

    #[test]
    fn wrapped_tool_notes_share_two_space_indent_and_are_not_output_body() {
        let palette =
            theme::Palette::from_capabilities(crate::config::PaletteMode::Dark, false, true);
        for text in [
            "取得 11:48:32 | 閉じて開くと再取得",
            "再取得中",
            "出力取得に失敗 | 閉じて開くと再試行",
            "読込中",
        ] {
            let lines = tool_note(text, 16, palette);
            assert!(!lines.is_empty());
            for line in lines {
                let shown = line
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>();
                assert!(shown.starts_with("  "), "{shown}");
                assert!(line.width() <= 16);
                assert_eq!(body_spans(&line).count(), 0);
                assert!(
                    line.spans
                        .iter()
                        .all(|s| s.style.fg == Some(palette.muted()))
                );
            }
        }
    }
    #[test]
    fn compact_shell_metadata_uses_shared_palette_without_changing_text() {
        use crate::config::PaletteMode;
        for (status, code, kind) in [
            ("completed", Some(0), RowKind::Success),
            ("exited", Some(7), RowKind::Failure),
            ("failed", None, RowKind::Failure),
            ("cancelled", None, RowKind::Cancelled),
        ] {
            let mut value = tool("metadata", status);
            value.exit_code = code;
            value.elapsed_ms = Some(12400);
            let mut previous_text = None;
            for mode in [PaletteMode::Dark, PaletteMode::Light] {
                for no_color in [false, true] {
                    let palette = theme::Palette::from_capabilities(mode, no_color, true);
                    let line = compact_tool_line(UiLanguage::Ja, &value, 80, palette);
                    let right = line.spans.last().unwrap();
                    assert!(right.content.ends_with("12.4秒"));
                    assert_eq!(right.style.fg, Some(kind.metadata_color(palette)));
                    let text = line
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>();
                    if let Some(previous) = &previous_text {
                        assert_eq!(&text, previous);
                    }
                    previous_text = Some(text);
                }
            }
        }
    }
    #[test]
    fn live_time_paints_from_saved_base_without_changing_layout_or_sources() {
        let now = std::time::Instant::now();
        let mut live = tool("live", "running");
        live.current_state = Some("background_current".into());
        live.background = true;
        live.started_at = Some("2026-10-31T00:00:00Z".into());
        live.elapsed_observation = Some((std::time::Duration::from_secs(316), now));
        live.reason_kind = crate::presentation::ToolReasonKind::HumanBackgrounded;
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![node(
            "live",
            "",
            vec![PresentationBlock {
                kind: BlockKind::ToolCall,
                tool: Some(live),
                ..Default::default()
            }],
        )];
        let (lines, _, sources) = history(&view, 80);
        // The source node may have less correlation detail than the cached row.
        view.nodes_mut_for_test()[0].presentation.blocks[0]
            .tool
            .as_mut()
            .unwrap()
            .reason_kind = crate::presentation::ToolReasonKind::Unknown;
        let mut painted = borrowed(&lines);
        paint_live_times(
            &mut painted,
            &lines,
            &sources,
            &view,
            80,
            now + std::time::Duration::from_secs(7),
        );
        let shown = painted
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(shown.contains("⏵ printf synthetic"), "{shown}");
        assert!(shown.contains("背景（利用者） | 5分23秒"), "{shown}");
        assert!(shown.contains("5分23秒"), "{shown}");
        assert_eq!(
            painted
                .iter()
                .find(|line| line
                    .spans
                    .last()
                    .is_some_and(|s| s.content.ends_with("5分23秒")))
                .unwrap()
                .spans
                .last()
                .unwrap()
                .style
                .fg,
            Some(RowKind::Running.metadata_color(view.palette))
        );
        assert!(
            plain(&lines).contains("経過未取得"),
            "cached rows must remain immutable"
        );
    }
    #[test]
    fn notification_region_keeps_sources_without_triggering_output_hydration() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        let live = tool("live", "completed");
        let execution = live.execution_id.clone().unwrap();
        let call_id = live.call_node_id.clone().unwrap();
        let mut event = node("notice", "", vec![]);
        event.presentation.kind = PresentationKind::Event;
        event.presentation.fields = vec![crate::presentation::PresentationField {
            key: "/payload/execution_id".into(),
            label: "実行ID".into(),
            value: execution.clone(),
            display: None,
            value_omitted: false,
        }];
        *view.nodes_mut_for_test() = vec![
            node(
                "call",
                "",
                vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(live),
                    ..Default::default()
                }],
            ),
            event,
        ];
        let region = node_region(&view, &view.nodes()[1]);
        assert!(
            region.execution_id.is_none(),
            "notification selection must not fetch shell output"
        );
        assert!(region.source_node_ids.contains(&call_id));
        assert!(region.source_node_ids.contains(&"notice".into()));
    }
    use super::*;
    use crate::presentation::{InputSource, NodePresentation, PresentationBlock, ToolPresentation};
    fn tool(group: &str, status: &str) -> ToolPresentation {
        ToolPresentation {
            group_id: Some(group.into()),
            call_id: "same".into(),
            command: "printf synthetic".into(),
            status: status.into(),
            exit_code: (status == "completed").then_some(0),
            call_node_id: Some("call".into()),
            result_node_id: Some("result".into()),
            execution_id: Some("execution".into()),
            ..Default::default()
        }
    }
    fn prose(text: &str, start: usize) -> PresentationBlock {
        PresentationBlock {
            kind: BlockKind::Prose,
            text: text.into(),
            source_start: start,
            source_end: start + text.chars().count(),
            source_exact: true,
            ..Default::default()
        }
    }
    fn node(id: &str, text: &str, blocks: Vec<PresentationBlock>) -> UiNode {
        UiNode {
            node_id: id.into(),
            role: "assistant".into(),
            text: text.into(),
            total_chars: text.chars().count(),
            presentation: NodePresentation {
                kind: PresentationKind::Assistant,
                blocks,
                ..Default::default()
            },
            ..Default::default()
        }
    }
    #[test]
    fn nonadjacent_tool_item_keeps_mixed_prose_and_canonical_frontier() {
        let call = PresentationBlock {
            kind: BlockKind::ToolCall,
            tool: Some(tool("group-a", "requested")),
            ..Default::default()
        };
        let result = PresentationBlock {
            kind: BlockKind::ToolResult,
            tool: Some(tool("group-a", "completed")),
            text: "SECRET_OUTPUT_CLOSED".into(),
            ..Default::default()
        };
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![
            node(
                "call",
                "BEFORE AFTER",
                vec![prose("BEFORE", 0), call, prose("AFTER", 7)],
            ),
            node("middle", "INTERMEDIATE", vec![prose("INTERMEDIATE", 0)]),
            node("result", "canonical result", vec![result]),
        ];
        let (lines, _, sources) = history(&view, 80);
        let text = plain(&lines);
        assert_eq!(text.matches("printf synthetic").count(), 1);
        assert!(!text.contains("SECRET_OUTPUT_CLOSED"));
        assert!(text.find("BEFORE").unwrap() < text.find("printf synthetic").unwrap());
        assert!(text.find("printf synthetic").unwrap() < text.find("AFTER").unwrap());
        assert!(text.find("AFTER").unwrap() < text.find("INTERMEDIATE").unwrap());
        let tool_row = sources
            .iter()
            .position(|row| {
                matches!(
                    row.item.as_deref().map(|item| &item.key),
                    Some(ItemKey::Tool { .. })
                )
            })
            .unwrap();
        assert_eq!(sources[tool_row].node, 0);
        assert_eq!(
            sources[tool_row].covered_end, 1,
            "header cannot span over intermediate prose"
        );
        assert_eq!(
            sources.last().unwrap().covered_end,
            3,
            "zero-row result advances only the traversed tail"
        );
        let item = sources[tool_row].item.as_ref().unwrap();
        assert_eq!(item.source_node_ids, ["call", "result"]);
        assert_eq!(item.primary_node_id, "result");
    }
    #[test]
    fn canonical_result_arrival_preserves_effective_outcome_and_received_inline_preview() {
        let mut call = tool("group-a", "requested");
        call.current_state = Some("exited".into());
        call.current_exit_code = Some(0);
        let mut result = tool("group-a", "completed");
        result.current_state = Some("exited".into());
        result.current_exit_code = Some(0);
        let outcome = tool_outcome(&call);
        assert_eq!(
            outcome,
            tool_outcome(&result),
            "metadata hydration cannot change the observed terminal outcome"
        );
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![
            node(
                "call",
                "call source",
                vec![PresentationBlock {
                    kind: BlockKind::ToolCall,
                    tool: Some(call),
                    ..Default::default()
                }],
            ),
            node(
                "result",
                "CANONICAL_OUTPUT",
                vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    tool: Some(result),
                    text: "CANONICAL_OUTPUT".into(),
                    source_exact: true,
                    source_end: 16,
                    ..Default::default()
                }],
            ),
        ];
        let key = ItemKey::Tool {
            group_id: "group-a".into(),
            call_id: "same".into(),
        };
        view.items.toggle(key.clone(), false, &outcome);
        assert!(plain(&history(&view, 80).0).contains("CANONICAL_OUTPUT"));
        let request = view.items.request(&key, &outcome);
        assert!(view.items.accept_preview(
            &key,
            request,
            super::super::history_items::Preview {
                blocks: vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    stream: Some(OutputStream::Stdout),
                    text: "RAW_AFTER_REPLY".into(),
                    ..Default::default()
                }],
                notice: None,
                refresh_error: None,
            },
            64 * 1024
        ));
        let shown = plain(&history(&view, 80).0);
        assert!(
            shown.contains("RAW_AFTER_REPLY"),
            "a received explicit preview must remain rendered when canonical source is nonadjacent"
        );
        assert!(!shown.contains("CANONICAL_OUTPUT"));
        assert_eq!(shown.matches("printf synthetic").count(), 1);
        assert!(view.items.is_open(&key, false, &outcome));
    }
    #[test]
    fn saved_context_lines_render_no_rows_in_plain_or_typed_history() {
        for typed in [false, true] {
            let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
            let mut first = node("first", "FIRST USER", vec![]);
            first.role = "user".into();
            first.presentation.kind = PresentationKind::User;
            let mut nodes = vec![first];
            if typed {
                let mut event = node("event", "saved observation", vec![]);
                event.role = "event".into();
                event.presentation.kind = PresentationKind::Event;
                nodes.push(event);
            }
            let mut context = node("context", "Shellboy context v1: thread=t", vec![]);
            context.role = "context".into();
            nodes.push(context);
            nodes.push(node("answer", "FINAL ANSWER", vec![]));
            *view.nodes_mut_for_test() = nodes;
            let shown = plain(&history(&view, 100).0);
            assert!(shown.contains("FIRST USER") && shown.contains("FINAL ANSWER"));
            assert!(!shown.contains("Shellboy context"), "{typed}: {shown}");
        }
    }
    #[test]
    fn repeated_quiet_facts_coalesce_with_counts_and_latest_references_without_losing_answers() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        let mut first = node("first", "FIRST USER", vec![]);
        first.role = "user".into();
        first.presentation.kind = PresentationKind::User;
        let mut nodes = vec![first];
        for index in 1..=120 {
            let mut quiet = node(&format!("event-{index}"), "saved observation", vec![]);
            quiet.role = "event".into();
            quiet.presentation.kind = PresentationKind::Event;
            quiet.presentation.title = if index % 2 == 0 {
                "AGENTS.md none"
            } else {
                "index not configured"
            }
            .into();
            quiet
                .presentation
                .fields
                .push(crate::presentation::PresentationField {
                    key: "/kind".into(),
                    value: if index % 2 == 0 {
                        "instructions_observed"
                    } else {
                        "index_state"
                    }
                    .into(),
                    ..Default::default()
                });
            nodes.push(quiet);
        }
        nodes.push(node("answer", "FINAL ANSWER", vec![]));
        *view.nodes_mut_for_test() = nodes;
        let (lines, _, sources) = history(&view, 100);
        let shown = plain(&lines);
        assert!(shown.contains("FIRST USER") && shown.contains("FINAL ANSWER"));
        let multiplier = crate::ui::text_width::glyphs::select("×", "x");
        assert!(
            shown.contains(&format!("⌘ {multiplier}60"))
                && shown.contains(&format!("⌕ {multiplier}60")),
            "{shown}"
        );
        let groups = sources
            .iter()
            .filter_map(|source| source.item.as_ref())
            .filter(|item| item.primary_node_id.starts_with("event-"))
            .map(|item| item.key.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            groups.len(),
            1,
            "two repeated facts produce one compact group, not sixty groups"
        );
        let group = sources
            .iter()
            .filter_map(|source| source.item.as_ref())
            .find(|item| item.primary_node_id.starts_with("event-"))
            .unwrap();
        assert_eq!(group.source_node_ids.first().unwrap(), "event-1");
        assert_eq!(group.source_node_ids.last().unwrap(), "event-120");
        assert_eq!(group.source_node_ids.len(), 120);
        assert_eq!(sources.first().unwrap().node, 0);
        assert_eq!(sources.last().unwrap().covered_end, 122);
        assert_eq!(
            view.nodes().len(),
            122,
            "all immutable source records remain available through originals"
        );
        assert!(!shown.contains("状態変更") && !shown.contains("通常通知"));
    }
    #[test]
    fn canonical_projection_bounds_include_a_quiet_prefix_hidden_by_tail_grouping() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        let nodes = (0..270)
            .map(|index| {
                let mut value = node(
                    &format!("q{index}"),
                    if index == 269 {
                        "FINAL ANSWER"
                    } else {
                        "saved observation"
                    },
                    vec![],
                );
                if index < 269 {
                    value.presentation.kind = PresentationKind::Event;
                    value.presentation.title = "AGENTS.md none".into();
                    value.presentation.ui_messages.title = Some(Message::EventTitle {
                        kind: "index_state".into(),
                    });
                    value.role = "event".into();
                }
                value
            })
            .collect();
        *view.nodes_mut_for_test() = nodes;
        view.navigation.scroll.focus = Some("q269".into());
        let (begin, end) = projection_bounds(&view);
        assert_eq!(end, 270);
        assert_eq!(end - begin, 128);
        let (lines, _, sources) = history(&view, 80);
        assert!(plain(&lines).contains("FINAL ANSWER"));
        assert_eq!(sources.first().unwrap().node, begin);
        assert_eq!(sources.first().unwrap().covered_end, 269);
        assert!(matches!(
            sources.first().unwrap().item.as_ref().unwrap().key,
            ItemKey::NoticeGroup(_)
        ));
        assert_eq!(begin.checked_sub(1), Some(141));
    }
    #[test]
    fn repeated_call_ids_in_other_groups_are_not_collapsed_together() {
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = ["a", "b"]
            .iter()
            .map(|group| {
                node(
                    group,
                    "",
                    vec![PresentationBlock {
                        kind: BlockKind::ToolCall,
                        tool: Some(tool(group, "completed")),
                        ..Default::default()
                    }],
                )
            })
            .collect();
        let (lines, _, sources) = history(&view, 80);
        assert_eq!(plain(&lines).matches("printf synthetic").count(), 2);
        let keys = sources
            .iter()
            .filter_map(|row| row.item.as_ref().map(|item| item.key.clone()))
            .collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), 2);
    }
    #[test]
    fn input_band_parts_override_only_background_and_omit_metadata_before_body() {
        use crate::config::PaletteMode;
        let mut value = node("external", "code link", vec![]);
        value.role = "user".into();
        value.presentation.input_source = InputSource::Unknown;
        value.presentation.input_sender_label = Some("Claude（レビュア）".into());
        value.presentation.created_at = Some("2026-10-31T12:34:00Z".into());
        let link = crate::terminal_links::marker_color(0);
        let code_style = Style::default()
            .fg(Color::Yellow)
            .bg(Color::Black)
            .add_modifier(Modifier::BOLD);
        let link_style = Style::default()
            .fg(Color::Blue)
            .bg(Color::Black)
            .add_modifier(Modifier::UNDERLINED)
            .underline_color(link);
        for mode in [PaletteMode::Dark, PaletteMode::Light] {
            for no_color in [false, true] {
                let palette = theme::Palette::from_capabilities(mode, no_color, true);
                let mark = Span::styled(
                    input_marker(&value),
                    Style::default().underline_color(INPUT_DECORATION_MARKER),
                );
                let mut lines = vec![
                    Line::from(vec![
                        mark,
                        Span::styled("code ", code_style),
                        Span::styled("link", link_style),
                    ])
                    .style(
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Line::from("continuation"),
                ];
                user_band(UiLanguage::Ja, &mut lines, &value, 80, palette);
                let expected_bg = palette.user_band(InputSource::Unknown).bg;
                for line in &lines {
                    assert_eq!(line.width(), 80);
                    assert_eq!(line.style.bg, expected_bg);
                    assert!(line.spans.iter().all(|s| s.style.bg == expected_bg));
                }
                assert_eq!(lines[0].style.fg, Some(Color::Green));
                assert!(lines[0].style.add_modifier.contains(Modifier::ITALIC));
                let body = body_spans(&lines[0]).collect::<Vec<_>>();
                assert_eq!(
                    body.iter().map(|s| s.content.as_ref()).collect::<String>(),
                    "code link"
                );
                assert_eq!(body[0].style.fg, code_style.fg);
                assert!(body[0].style.add_modifier.contains(Modifier::BOLD));
                assert_eq!(body[1].style.underline_color, Some(link));
                assert!(body[1].style.add_modifier.contains(Modifier::UNDERLINED));
                assert!(plain(&lines).contains("Claude（レビュア） | 12:34"));
                assert!(!plain(&lines[1..]).contains("Claude"));
                assert_eq!(
                    lines[0].spans.last().unwrap().style.fg,
                    Some(palette.muted())
                );
                let mut narrow = vec![Line::from("untruncated")];
                user_band(UiLanguage::Ja, &mut narrow, &value, 11, palette);
                assert_eq!(plain(&narrow), "untruncated");
                assert_eq!(body_spans(&narrow[0]).count(), 1);
            }
        }
    }
    #[test]
    fn input_band_all_sources_keep_body_anchors_and_display_metadata_only_once() {
        for input in [
            InputSource::Human,
            InputSource::Unknown,
            InputSource::Parent,
            InputSource::SelfModel,
            InputSource::OtherModel,
            InputSource::Revisit,
        ] {
            let mut user = node("input", "first\nClaude | 12:34\nlast", vec![]);
            user.role = "user".into();
            user.presentation.kind = PresentationKind::User;
            user.presentation.input_source = input;
            user.presentation.input_sender_label = Some("Claude".into());
            user.presentation.created_at = Some("2026-10-31T12:34:00Z".into());
            let original = user.text.clone();
            let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
            *view.nodes_mut_for_test() = vec![user];
            let (lines, _, sources) = history(&view, 80);
            let rows = lines
                .iter()
                .zip(sources.iter())
                .filter(|(line, source)| source.item.is_some() && !line.spans.is_empty())
                .collect::<Vec<_>>();
            assert_eq!(
                rows.len(),
                3,
                "{input:?}: {:?}",
                rows.iter()
                    .map(|(line, source)| (line.to_string(), source.offset))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                rows.iter()
                    .map(|(_, source)| source.offset)
                    .collect::<Vec<_>>(),
                [0, 6, 21]
            );
            assert!(!plain(&lines).contains("入力 |"));
            assert_eq!(view.nodes()[0].text, original);
            for (i, (line, _)) in rows.iter().enumerate() {
                let body = body_spans(line)
                    .map(|s| s.content.as_ref())
                    .collect::<String>();
                assert_eq!(body, original.lines().nth(i).unwrap());
                assert_eq!(
                    line.spans
                        .iter()
                        .filter(|s| s.content.contains("Claude")
                            && s.style.underline_color == Some(INPUT_DECORATION_MARKER))
                        .count(),
                    usize::from(i == 0)
                );
            }
            assert!(
                borrowed(&lines)
                    .iter()
                    .flat_map(|line| &line.spans)
                    .all(|span| span.style.underline_color.is_none())
            );
        }
    }
    #[test]
    fn failed_tool_opens_reason_without_execution_and_user_bands_fill_wrapped_rows() {
        let mut failed = tool("failed", "cancelled");
        failed.execution_id = None;
        failed.reason = "未実行: 利用者がTurnを中断".into();
        let mut user = node(
            "human",
            "one two three four five six seven eight nine",
            vec![],
        );
        user.role = "user".into();
        user.presentation.kind = PresentationKind::User;
        user.presentation.input_source = InputSource::Human;
        user.presentation.created_at = Some("2026-10-30T12:34:56+09:00".into());
        let mut view = ThreadView::new(&japanese_config(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![
            user,
            node(
                "cancelled",
                "",
                vec![PresentationBlock {
                    kind: BlockKind::ToolResult,
                    tool: Some(failed),
                    ..Default::default()
                }],
            ),
        ];
        let (lines, _, sources) = history(&view, 24);
        assert!(plain(&lines).contains("未実行"));
        let user_rows = sources
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.item
                    .as_ref()
                    .is_some_and(|item| item.key == ItemKey::Node("human".into()))
            })
            .filter(|(index, _)| !lines[*index].spans.is_empty())
            .collect::<Vec<_>>();
        assert!(user_rows.len() > 1);
        for (index, _) in user_rows {
            assert_eq!(
                lines[index].width(),
                24,
                "every wrapped user row fills the band"
            );
        }
    }
}

#[cfg(test)]
mod code_caption_anchor_tests {
    use super::*;
    #[test]
    fn copy_labels_are_visible_but_never_count_as_canonical_source_or_links() {
        let source = "```rust\nlet one = 1;\n```\n";
        let payload = serde_json::json!({"role":"assistant","content":source});
        let mut projected = crate::presentation::project("assistant", &payload, source, 0);
        crate::presentation::render_markdown(&mut projected, 78);
        let lines = &projected.blocks[0].rendered.as_ref().unwrap().lines;
        let caption = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("[1] Copy")))
            .unwrap();
        assert_eq!(body_spans(caption).count(), 0);
        let shown = borrowed(std::slice::from_ref(caption));
        assert!(
            shown[0]
                .spans
                .iter()
                .any(|s| s.content.contains("[1] Copy"))
        );
        assert!(
            shown[0]
                .spans
                .iter()
                .all(|s| s.style.underline_color.is_none())
        );
        assert_eq!(crate::history::text_of_payload(&payload), source);
    }
}

#[cfg(test)]
#[path = "presentation/i18n_tests.rs"]
mod i18n_tests;

/// Shared ui.rs recovery banner bridge. Commands and provider text stay literal.
pub(super) fn recovery_lines(
    snapshot: &super::activity::ActivitySnapshot,
    language: UiLanguage,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let Some(recovery) = &snapshot.recovery else {
        return Vec::new();
    };
    let current = snapshot.phase == crate::activity::ActivityPhase::Recovery;
    let en = language == UiLanguage::En;
    let title = match (en, current) {
        (true, true) => "! Awaiting recovery | Waiting for your action".into(),
        (false, true) => "! 復旧待ち | 操作を待っています".into(),
        (true, false) => format!(
            "! Previous recovery wait | {}",
            crate::presentation::messages::phase_label(snapshot.phase, language)
        ),
        (false, false) => format!("! 前回の復旧待ち | {}", snapshot.phase.label()),
    };
    let actions = match (en, current) {
        (true, true) => {
            "/retry Retry same input | Type new instructions | Ctrl+C Stop | F8 Details"
        }
        (false, true) => "/retry 同じ内容で再試行 | 入力で新指示 | Ctrl+C 停止 | F8 詳細",
        (true, false) => "Type to start new work | F8 Run / turn details",
        (false, false) => "入力で新しい作業 | F8 作業/応答処理の詳細",
    };
    let mut essential = vec![Line::from(title)];
    essential.push(Line::from(if en {
        format!(
            "{} live shells | {} uncollected | Review side effects in details",
            super::activity::count_label(recovery.live_shells, recovery.counts_capped, language),
            super::activity::count_label(
                recovery.uncollected_shells,
                recovery.counts_capped,
                language
            )
        )
    } else {
        format!(
            "動作中shell {} | 回収未完了 {} | 副作用は詳細で確認",
            super::activity::count_label(recovery.live_shells, recovery.counts_capped, language),
            super::activity::count_label(
                recovery.uncollected_shells,
                recovery.counts_capped,
                language
            )
        )
    }));
    if let Some(facts) = &recovery.content_filter {
        let summary = crate::presentation::content_filter::summary(facts, language);
        essential.extend(
            hanging_wrap(vec![Line::from(summary)], width.max(1))
                .into_iter()
                .take(2),
        );
    }
    let actions = if current
        && recovery
            .content_filter
            .as_ref()
            .is_some_and(|facts| facts.is_content_filter())
    {
        crate::presentation::content_filter::compact_actions(language)
    } else {
        actions
    };
    // Never let optional provider/context/count detail displace recovery actions.
    essential.extend(hanging_wrap(vec![Line::from(actions)], width.max(1)));
    // A long retry footer must not conceal a saved quota/reset deadline.
    if let Some(limit) = recovery.limit_summary_localized(language) {
        essential.extend(hanging_wrap(vec![Line::from(limit)], width.max(1)));
    }
    let reason = crate::presentation::sanitize_plain(&recovery.message, 0)
        .0
        .replace('\n', " ");
    let mut details = vec![Line::from(format!("{} | {reason}", recovery.stage))];
    if let Some(reason) = &recovery.stop_reason {
        details.push(Line::from(match (en, reason.as_str()) {
            (true, "explicit_stop") => "Stopped by the user",
            (false, "explicit_stop") => "人による停止",
            (true, "shutdown" | "app_shutdown") => "Application shutdown",
            (false, "shutdown" | "app_shutdown") => "アプリ終了",
            (true, _) => "Stop reason: F8 Details",
            (false, _) => "停止理由はF8の詳細で確認",
        }));
    }
    details.push(Line::from(if en {
        format!(
            "Request saved | {} pending inputs | {} saved tool results",
            super::activity::count_label(
                recovery.pending_messages,
                recovery.counts_capped,
                language
            ),
            super::activity::count_label(
                recovery.saved_tool_results,
                recovery.counts_capped,
                language
            )
        )
    } else {
        format!(
            "依頼は保存済み | 未反映指示 {} | 保存済みツール結果 {}",
            super::activity::count_label(
                recovery.pending_messages,
                recovery.counts_capped,
                language
            ),
            super::activity::count_label(
                recovery.saved_tool_results,
                recovery.counts_capped,
                language
            )
        )
    }));
    if let Some(summary) = &snapshot.context_reduction {
        details.push(Line::from(summary.clone()));
    }
    let mut lines = hanging_wrap(essential, width.max(1));
    lines.extend(
        hanging_wrap(details, width.max(1))
            .into_iter()
            .take(usize::from(height).saturating_sub(lines.len())),
    );
    lines.truncate(usize::from(height));
    lines
}
