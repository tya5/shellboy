//! Display-only layout of worker-produced typed blocks and Markdown tokens.
use super::theme;
use super::{ThreadView, UiNode, rendered_node_text};
use crate::presentation::{BlockKind, OutputStream, PresentationKind};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
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
    text.trim_start_matches([' ', '›', '●', '│', '┃']).trim()
}

fn label(node: &UiNode) -> &str {
    if !node.presentation.title.is_empty() {
        return &node.presentation.title;
    }
    match node.role.as_str() {
        "user" => "あなた",
        "assistant" => "Shellboy",
        "developer" | "system" => "状態変更",
        "tool" => "実行結果",
        _ => "履歴",
    }
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
        PresentationKind::Event => theme::MUTED,
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
    node: &UiNode,
    expanded: bool,
    start: usize,
    width: u16,
    targets: &mut Vec<std::sync::Arc<str>>,
) -> NodeLines {
    let conversational = conversational(node);
    let typed_tools = node
        .presentation
        .blocks
        .iter()
        .any(|block| block.tool.is_some());
    let header =
        if conversational || node.presentation.kind == PresentationKind::ToolCall || typed_tools {
            Vec::new()
        } else {
            vec![Line::from(Span::styled(
                if node.presentation.kind == PresentationKind::ToolResult {
                    format!("↳ {}", label(node))
                } else {
                    match node.presentation.kind {
                        PresentationKind::Event => format!("· {}", label(node)),
                        PresentationKind::Summary => format!("◇ {}", label(node)),
                        PresentationKind::Spill => format!("↪ {}", label(node)),
                        PresentationKind::Interrupted => format!("⊘ {}", label(node)),
                        _ => label(node).to_owned(),
                    }
                },
                Style::default()
                    .fg(color(node))
                    .add_modifier(Modifier::BOLD),
            ))]
        };
    let mut out = NodeLines::new(header);
    if node.text.is_empty() && node.total_chars > 0 {
        out.push(Line::from("本文未キャッシュ · 詳細から原文を取得"));
        return out;
    }
    if node.presentation.blocks.is_empty() {
        out.role = LineRole::Body;
        if matches!(node.role.as_str(), "developer" | "system" | "tool") {
            out.push(Line::from(Span::styled(
                "詳細で内容・参照を確認",
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
    for block in &node.presentation.blocks {
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
                format!(
                    "▸ 推論 {}文字 · 折りたたみ /reasoning",
                    block.source_end.saturating_sub(block.source_start)
                ),
                Style::default().fg(theme::MUTED),
            )));
            continue;
        }
        if block.kind == BlockKind::Reasoning {
            out.push(Line::from(Span::styled(
                "▸ 推論（展開中）",
                Style::default().fg(theme::MUTED),
            )));
        }
        if block.kind == BlockKind::Event && block.text == node.presentation.title {
            continue;
        }
        if let Some(tool) = &block.tool {
            if previous_tool.as_deref() != Some(tool.call_id.as_str()) {
                out.extend(tool_header_lines(
                    tool,
                    block.kind == BlockKind::ToolCall,
                    width,
                    &node.presentation.links,
                    targets,
                ));
                if !tool.reason.is_empty() {
                    out.extend(hanging_wrap(
                        vec![Line::from(Span::styled(
                            format!("  ┊ {}", tool.reason),
                            Style::default().fg(theme::MUTED),
                        ))],
                        width,
                    ));
                }
            }
            previous_tool = Some(tool.call_id.clone());
            // Commands live in the call heading; do not print them a second time.
            if block.kind == BlockKind::ToolCall || block.text == "実行結果 · 詳細" {
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
                block.text.clone()
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
                                "↳ shell  "
                            } else {
                                "         "
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
                        format!("… 出力抜粋（表示{count}行中{}行省略）· F6 詳細", count - 5),
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
                            "  │ ",
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
    for notice in omission_notices(node) {
        out.push(Line::from(Span::styled(
            notice,
            Style::default().fg(theme::MUTED),
        )));
    }
    out
}
fn omission_notices(node: &UiNode) -> Vec<String> {
    let mut parts = Vec::new();
    let end = node
        .base_char_offset
        .saturating_add(node.text.chars().count())
        .min(node.total_chars);
    if node.base_char_offset > 0 || end < node.total_chars {
        parts.push(format!(
            "{} {}–{end}/{}文字",
            if node.presentation.provisional_stream {
                "生成中の表示範囲"
            } else {
                "原文の取得範囲"
            },
            node.base_char_offset.saturating_add(1),
            node.total_chars
        ));
    }
    if node.presentation.body_omitted_chars > 0 {
        parts.push(format!(
            "本文・出力{}文字省略",
            node.presentation.body_omitted_chars
        ));
    }
    if node.presentation.body_extent_unknown {
        parts.push(
            if node.presentation.provisional_stream {
                "途中表示の更新欠落を検出。確定本文の保存後に再取得"
            } else {
                "出力の表示範囲・省略量は未確認"
            }
            .into(),
        );
    }
    if parts.is_empty() {
        Vec::new()
    } else {
        vec![format!("{} · F6 詳細", parts.join(" · "))]
    }
}
fn tool_header_lines(
    tool: &crate::presentation::ToolPresentation,
    request: bool,
    width: u16,
    links: &[crate::presentation::PresentationLink],
    targets: &mut Vec<std::sync::Arc<str>>,
) -> Vec<Line<'static>> {
    let mut header = vec![Line::from(Span::styled(
        if request {
            tool.call_heading()
        } else {
            tool.heading()
        },
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
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        widgets::{Paragraph, Widget, Wrap},
    };
    use unicode_width::UnicodeWidthStr;
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
fn node_lines(
    node: &UiNode,
    expanded: bool,
    start: usize,
    width: u16,
    links: &mut Vec<std::sync::Arc<str>>,
) -> Vec<Line<'static>> {
    let conversational = conversational(node);
    let tool_only = !node.presentation.blocks.is_empty()
        && node
            .presentation
            .blocks
            .iter()
            .all(|block| block.kind == BlockKind::ToolCall);
    let mut source = node_lines_inner(node, expanded, start, width.saturating_sub(2).max(1), links);
    // Preserve the legacy empty conversational placeholder: removing it changes
    // page boundary row counts when an uncached/unknown node is reprojected.
    if source.rows.is_empty() && conversational && !tool_only {
        source.role = LineRole::Body;
        source.push(Line::default());
    }
    let mut lines = Vec::new();
    let mut body_marked = false;
    for (role, mut line) in source.rows {
        annotate_links(
            std::slice::from_mut(&mut line),
            &node.presentation.links,
            links,
        );
        let rows = if conversational && !tool_only {
            hanging_wrap(vec![line], width.saturating_sub(2).max(1))
        } else {
            vec![line]
        };
        for mut row in rows {
            if conversational && !tool_only {
                let marker = if matches!(role, LineRole::Body) && !body_marked {
                    body_marked = true;
                    Span::styled(
                        if node.role == "user" { "› " } else { "● " },
                        Style::default().fg(if node.role == "user" {
                            theme::USER_MARKER
                        } else {
                            theme::ASSISTANT_MARKER
                        }),
                    )
                } else {
                    if row.spans.is_empty() {
                        lines.push(row);
                        continue;
                    }
                    Span::raw("  ")
                };
                row.spans.insert(0, marker);
            }
            lines.push(row);
        }
    }
    if !conversational
        && !tool_only
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
        for mut line in hanging_wrap(body, width.saturating_sub(2).max(1)) {
            line.spans.insert(0, Span::raw("  "));
            lines.push(line);
        }
    }
    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    lines
}
fn quiet_event(node: &UiNode) -> bool {
    // Received audit previews retain their paid body even after recovery ends.
    node.role != "received_response"
        && node.presentation.kind == PresentationKind::Event
        && !node.presentation.requires_attention()
}

fn event_group(nodes: &[UiNode]) -> Vec<Line<'static>> {
    let routine = nodes
        .iter()
        .filter(|node| node.presentation.event_class == crate::presentation::EventClass::Routine)
        .count();
    let mut title = format!("· 状態変更 {}件", nodes.len());
    let unknown = nodes
        .iter()
        .filter(|node| node.presentation.event_class == crate::presentation::EventClass::Unknown)
        .count();
    if unknown > 0 {
        title.push_str(&format!(" · その他{unknown}件"));
    }
    if routine > 0 {
        title.push_str(&format!(" · 通常通知{routine}件"));
    }
    title.push_str(" · F6 詳細");
    let mut lines = vec![Line::from(Span::styled(
        title,
        Style::default().fg(theme::MUTED),
    ))];
    for node in nodes
        .iter()
        .filter(|node| node.presentation.event_class == crate::presentation::EventClass::Change)
    {
        lines.push(Line::from(Span::styled(
            format!("  · {}", label(node)),
            Style::default().fg(theme::TEXT),
        )));
    }
    lines
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
                    | "legacy_pending_operations_with_unknown_targets"
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
#[derive(Clone, Copy, Debug)]
pub(super) struct RowSource {
    pub node: usize,
    /// Exclusive end of the source nodes represented by this physical row.
    /// Event groups retain their first-node anchor while covering every member.
    pub covered_end: usize,
    pub offset: usize,
}
pub(super) fn history(
    view: &ThreadView,
    width: u16,
) -> (Vec<Line<'static>>, Vec<std::sync::Arc<str>>, Vec<RowSource>) {
    let mut links = Vec::new();
    let mut lines = Vec::new();
    let mut sources = Vec::new();
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
    let mut index = begin;
    while index < end {
        let begin = index;
        let mut rendered_index = index;
        let mut group_header_rows = 0;
        let node = &view.nodes()[index];
        let chunk = if let Some(summary) = interruption_group(&view.nodes()[index..end]) {
            index += summary + 1;
            rendered_index = index - 1;
            let summary = &view.nodes()[rendered_index];
            let mut chunk = node_lines(summary, false, summary.base_char_offset, width, &mut links);
            group_header_rows = chunk.first().map_or(0, |line| {
                hanging_wrap(vec![line.clone()], width.max(1)).len()
            });
            chunk.push(Line::from(Span::styled(
                format!("  原文{}件 · F6 詳細", index - begin),
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
                    tool,
                    false,
                    width.saturating_sub(2).max(1),
                    &result.presentation.links,
                    &mut links,
                );
                group_header_rows = hanging_wrap(header, width.max(1)).len();
            }
            let mut chunk = node_lines(result, false, result.base_char_offset, width, &mut links);
            // Result commands are enriched from their exact call source. Retain
            // its explicit URL targets when rendering that shared heading.
            annotate_links(&mut chunk, &node.presentation.links, &mut links);
            chunk
        } else if quiet_event(node) {
            index += 1;
            while index < end && quiet_event(&view.nodes()[index]) {
                index += 1;
            }
            event_group(&view.nodes()[begin..index])
        } else {
            index += 1;
            node_lines(
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
        let chunk = hanging_wrap(chunk, width.max(1));
        let node = &view.nodes()[rendered_index];
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
            let text = line
                .spans
                .iter()
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
                line.spans.iter().find_map(|span| {
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
                                    line.spans.iter().find_map(|span| {
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
                    for span in &line.spans {
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
                node: rendered_index,
                covered_end: index,
                offset,
            });
            lines.push(line);
        }
        sources.push(RowSource {
            node: rendered_index,
            covered_end: index,
            offset,
        });
        lines.push(Line::default());
    }
    (lines, links, sources)
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
mod tests {
    use super::*;
    #[test]
    fn formal_shutdown_group_identity_survives_generic_field_limits() {
        fn scene(value: &serde_json::Value, summary_kind: &str) -> Vec<UiNode> {
            let mut nodes = Vec::new();
            for section in [
                "runs",
                "turns",
                "shells",
                "owned_children",
                "legacy_pending_operations_with_unknown_targets",
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
        let small:serde_json::Value=serde_json::from_str(r#"{"actor":{"kind":"shellboy","source":"shutdown"},"change":{"automatic_replay":false,"cancelled_waits":{"count":0,"detail_node_count":0,"detail_node_ids":[],"details_kind":"interruption_details","history_kind":"fixture","items":[],"section":"cancelled_waits","truncated":false},"legacy_pending_operations_with_unknown_targets":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-00"],"details_kind":"interruption_details","items":[{"operation_id":"fixture","request_kind":"fixture","state":"fixture","wait_targets":"fixture"}],"section":"legacy_pending_operations_with_unknown_targets","truncated":false},"os_processes_inspected":false,"owned_children":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-01"],"details_kind":"interruption_details","items":[{"answer_node_id":"fixture-id-02","first_message_id":"fixture-id-03","first_request_node_id":"fixture-id-04","last_turn_id":"fixture-id-05","owner_run_id":"fixture-id-06","planned_state":"stopped","previous_state":"live","run_id":"fixture-id-07","thread_id":"fixture-id-08"}],"section":"owned_children","truncated":false},"reason":"app_shutdown","result_uncertain":true,"runs":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-09"],"details_kind":"interruption_details","items":[{"first_message_id":"fixture-id-10","first_request_node_id":"fixture-id-11","owner_run_id":null,"planned_state":"stopped","previous_state":"live","request_count":1,"run_id":"fixture-id-06","stop_reason":"app_shutdown"}],"section":"runs","truncated":false},"shells":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-12"],"details_kind":"interruption_details","items":[{"execution_id":"fixture-id-13","output_complete":false,"planned_state":"stopped","previous_state":"running","result_uncertain":true,"retention_generation":0,"run_id":"fixture-id-06","saved_sequence":1,"turn_id":"fixture-id-14"}],"section":"shells","truncated":false},"turns":{"count":1,"detail_node_count":1,"detail_node_ids":["fixture-id-15"],"details_kind":"interruption_details","items":[{"answer_node_id":null,"previous_state":"running","recovery_reason":null,"run_id":"fixture-id-06","turn_id":"fixture-id-14"}],"section":"turns","truncated":false},"unreflected_messages":{"count":0,"detail_node_count":0,"detail_node_ids":[],"details_kind":"interruption_details","items":[],"section":"unreflected_messages","truncated":false}},"kind":"app_shutdown","observed_at":"fixture","snapshot_node_id":"fixture-id-16","timezone":"fixture"}"#).unwrap();
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
        let gap = "… 出力抜粋（表示120行中115行省略）· F6 詳細";
        let body = (0..120)
            .map(|index| {
                if index == 50 {
                    gap.to_owned()
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
        let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![node];
        let (lines, _, sources) = history(&view, 100);
        let head_row = lines
            .iter()
            .position(|line| plain(std::slice::from_ref(line)).contains("LINE002"))
            .unwrap();
        let head_offset = body[..body.find("LINE002").unwrap()].chars().count();
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
            "… 出力抜粋（表示120行中115行省略）· F6 詳細",
            Style::default()
                .fg(theme::MUTED)
                .underline_color(EXCERPT_GAP_MARKER),
        ));
        let mut rows = hanging_wrap(vec![gap], 16);
        assert!(rows.len() > 1);
        for row in &mut rows {
            row.spans
                .insert(0, Span::styled("  │ ", Style::default().fg(theme::MUTED)));
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
                "│ ",
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
        let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![call, result];
        let (lines, _, sources) = history(&view, 22);
        let first_body = lines
            .iter()
            .position(|line| {
                line.spans.iter().any(|span| {
                    span.style.fg == Some(theme::TEXT) && span.content.contains("MATCH_BODY")
                })
            })
            .unwrap();
        assert!(
            first_body > 1,
            "the long command header has physical continuation rows"
        );
        assert_eq!(sources[0].node, 0);
        assert!(
            sources[1..first_body]
                .iter()
                .all(|source| source.node == 1 && source.offset == 123)
        );
        assert_eq!(sources[first_body].node, 1);
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
            let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
            view.thread_id = Some("thread".into());
            *view.nodes_mut_for_test() = vec![leading, rendered];
            view.total_nodes = 2;
            view.follow = false;
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
            view.anchor = crate::ui::ViewAnchor {
                node_id: Some(actual.into()),
                char_offset: expected,
            };
            let mut cache = super::super::HistoryLayoutCache::default();
            let budget = 1024 * 1024;
            let layout = view.history_layout("fixture", &mut cache, budget, 100, 2);
            view.viewport_row(&layout, 100, 2);
            assert!(view.scroll(1, "fixture", &mut cache, budget).is_empty());
            assert_eq!(view.anchor.node_id.as_deref(), Some(actual));
            let expected_next = base + body[..body.find(next).unwrap()].chars().count();
            assert_eq!(view.anchor.char_offset, expected_next);
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
        let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![call];
        view.nodes_mut_for_test().extend(results);
        let shown = plain(&history(&view, 120).0);
        let requests = shown
            .lines()
            .filter(|line| line.trim_start().starts_with('↳'))
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|line| !line.contains("終了") && !line.contains("呼出し"))
        );
        assert_eq!(shown.matches("終了 0").count(), 1);
        assert_eq!(shown.matches("終了 7").count(), 1);
        assert!(shown.contains("#1 echo first") && shown.contains("#2 echo second"));
        assert_eq!(view.nodes().len(), 3);
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
        let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
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
        let mut view = ThreadView::new(&crate::config::Config::default(), std::env::temp_dir());
        *view.nodes_mut_for_test() = vec![call.clone(), result.clone()];
        let (lines, targets, sources) = history(&view, 100);
        assert_eq!(plain(&lines).matches(url).count(), 1);
        assert!(
            sources.iter().all(|source| source.covered_end == 2)
                && sources[0].node == 0
                && sources.iter().skip(1).all(|source| source.node == 1)
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
            2,
            "mixed prose is not collapsed"
        );
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
            2,
            "separated results do not erase call rows"
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
        let shown = plain(&node_lines(&node, false, 0, 80, &mut Vec::new()));
        assert!(!shown.contains("詳細情報は"));
        assert!(omission_notices(&node).is_empty());
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
        let notices = omission_notices(&node).join("\n");
        assert!(notices.contains("原文の取得範囲 51–55/100文字"));
        node.presentation.body_omitted_chars = 17;
        node.presentation.body_extent_unknown = true;
        let notices = omission_notices(&node);
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
        let shown = plain(&node_lines(&node, false, 0, 40, &mut Vec::new()));
        assert!(shown.contains("future_node"));
        assert!(
            !shown.contains('●'),
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
                let lines = node_lines(&node, false, 0, 24, &mut targets);
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
                    let lines = node_lines(&typed, false, 0, 24, &mut targets);
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
        let lines = node_lines(&node, false, 0, width, &mut targets);
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
        let lines = node_lines(&node, false, 0, 20, &mut targets);
        assert!(plain(&lines).starts_with("● "));
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
            let lines = node_lines(&node, false, 0, 50, &mut Vec::new());
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
                    assert!(
                        code_cells
                            .iter()
                            .filter(|cell| cell.symbol() == "s")
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
            let shown = node_lines(&node, expanded, 0, 80, &mut Vec::new());
            let shown = plain(&shown);
            assert!(
                shown
                    .lines()
                    .any(|line| line.trim_start().starts_with("▸ 推論"))
            );
            assert!(shown.lines().any(|line| line.starts_with("● 回答の本文")));
            assert!(!shown.lines().any(|line| line.starts_with("● 推論")));
        }
        let event = |kind: &str, change: serde_json::Value| {
            let payload = serde_json::json!({"text":format!("Shellboy event v1: {}",serde_json::json!({"kind":kind,"change":change}))});
            let text = crate::history::text_of_payload(&payload);
            UiNode {
                node_id: kind.into(),
                role: "shellboy".into(),
                text: text.clone(),
                base_char_offset: 0,
                total_chars: text.chars().count(),
                reasoning_ranges: vec![],
                presentation: crate::presentation::project("event", &payload, &text, 0),
            }
        };
        let events = vec![
            event("run_started", serde_json::json!({})),
            event("turn_started", serde_json::json!({})),
            event(
                "thread_settings",
                serde_json::json!({"before":{"model":"A"},"after":{"model":"B"}}),
            ),
        ];
        let shown = plain(&event_group(&events));
        assert!(shown.contains("· 状態変更 3件"));
        assert!(shown.contains("通常通知2件"));
        assert!(shown.contains("モデル: A → B"));
        assert!(!shown.contains("作業開始 / 応答処理を開始"));
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
            plain(&node_lines(&node, false, 0, 80, &mut Vec::new()))
                .starts_with("◇ 要約 · 出典2件")
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
        let shown = node_lines(&node, false, 0, 80, &mut Vec::new());
        assert_eq!(plain(&shown), "↳ shell  printf synthetic-marker");
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
                title: "shell · exited · 失敗 exit 101".into(),
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
            let shown = node_lines(&node, false, 0, width, &mut Vec::new());
            assert!(plain(&shown).contains("出力 00") && plain(&shown).contains("出力 39"));
            assert!(!plain(&shown).contains("出力 20"));
            assert!(plain(&shown).contains("抜粋") && plain(&shown).contains("F6 詳細"));
            assert!(!plain(&shown[..1]).contains("stdout"));
            assert_eq!(shown[0].spans[0].style.fg, Some(Color::LightRed));
            assert!(plain(&shown[1..]).starts_with("  │ 出力 00"));
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
                title: "shell · exited".into(),
                blocks,
                ..Default::default()
            },
        };
        let lines = node_lines(&node, false, 0, 35, &mut Vec::new());
        let text = plain(&lines);
        assert!(!text.contains("stdout") && !text.contains("stderr") && !text.contains("Combined"));
        assert!(lines.iter().all(|line| line.width() <= 35));
        let colors = lines
            .iter()
            .filter_map(|line| {
                line.spans
                    .first()
                    .filter(|span| span.content == "  │ ")
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
            (None, "·", Color::Cyan),
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
                let lines = node_lines(&node, false, 0, 160, &mut Vec::new());
                let header = lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .find(|span| span.content.contains("#2 printf fixture"))
                    .unwrap();
                assert!(
                    header.content.starts_with(&format!(
                        "{} #2 printf fixture",
                        if kind == "assistant_response" {
                            "↳"
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
