//! Shared popup geometry, list focus, and reader viewports. No domain actions or I/O.
use super::theme;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

/// The existing application status remains visible below every popup.
pub(super) const STATUS_ROWS: u16 = 2;
pub(super) fn viewport(area: Rect) -> Rect {
    Rect {
        height: area.height.saturating_sub(STATUS_ROWS),
        ..area
    }
}

/// Display-only item data stays inspectable until converted to the OSS widget.
/// Source values and copy/execute targets remain owned by each screen.
pub(super) struct Item {
    lines: Vec<Line<'static>>,
    style: Style,
}
impl Item {
    pub fn new<T: Into<Text<'static>>>(text: T) -> Self {
        let mut lines = text.into().lines;
        if lines.is_empty() {
            lines.push(Line::default());
        }
        Self {
            lines,
            style: Style::default(),
        }
    }
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
    pub fn preview(mut self, height: usize) -> Self {
        let height = height.max(1);
        if self.lines.len() > height {
            // Keep the field label and editing tail, rather than letting List
            // omit an oversized item altogether. Ellipsis is display-only.
            let tail = self
                .lines
                .split_off(self.lines.len() - height.saturating_sub(1).max(1));
            self.lines.truncate(usize::from(height > 1));
            let mut tail = tail;
            if let Some(line) = tail.first_mut() {
                use unicode_segmentation::UnicodeSegmentation;
                use unicode_width::UnicodeWidthStr;
                let mut columns = 2usize;
                for span in &mut line.spans {
                    let mut byte = 0;
                    for grapheme in span.content.graphemes(true) {
                        if columns == 0 {
                            break;
                        }
                        byte += grapheme.len();
                        columns = columns.saturating_sub(grapheme.width());
                    }
                    span.content = span.content[byte..].to_owned().into();
                    if columns == 0 {
                        break;
                    }
                }
                line.spans
                    .insert(0, Span::styled("… ", Style::default().fg(theme::MUTED)));
            }
            self.lines.extend(tail);
        }
        self
    }
    fn bounded(self, height: usize) -> ListItem<'static> {
        let item = self.preview(height);
        ListItem::new(item.lines).style(item.style)
    }
}

/// The reader owns its scroll offset; this helper only interprets movement keys.
pub(super) fn reader_key(key: KeyCode, scroll: u16, page: u16) -> Option<u16> {
    match key {
        KeyCode::Down => Some(scroll.saturating_add(1)),
        KeyCode::Up => Some(scroll.saturating_sub(1)),
        KeyCode::PageDown => Some(scroll.saturating_add(page.max(1))),
        KeyCode::PageUp => Some(scroll.saturating_sub(page.max(1))),
        _ => None,
    }
}
pub(super) fn selected_style() -> Style {
    Style::default()
        .fg(theme::SELECTION_FOREGROUND)
        .bg(theme::SELECTION_BACKGROUND)
        .add_modifier(Modifier::BOLD)
}
pub(super) fn normal_style() -> Style {
    Style::default().fg(theme::TEXT)
}
pub(super) fn panel(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    context: &str,
    footer: &str,
) -> Rect {
    panel_height(frame, area, title, context, footer, None)
}
pub(super) fn compact(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    context: &str,
    footer: &str,
    height: u16,
) -> Rect {
    panel_height(frame, area, title, context, footer, Some(height))
}
struct PanelLayout {
    outer: Rect,
    padded: Rect,
    footer_rows: u16,
    divider: u16,
    header_rows: u16,
}
impl PanelLayout {
    fn new(area: Rect, context: &str, footer: &str, height: Option<u16>) -> Self {
        let width = area
            .width
            .saturating_sub(if area.width >= 12 { 4 } else { 0 })
            .min(112);
        let available = area
            .height
            .saturating_sub(if area.height >= 10 { 4 } else { 0 });
        let height = height.unwrap_or(available).min(available);
        let outer = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        let inner = Block::default().borders(Borders::ALL).inner(outer);
        let padded = if inner.width >= 5 {
            Rect::new(inner.x + 1, inner.y, inner.width - 2, inner.height)
        } else {
            inner
        };
        let footer_rows = if padded.width == 0 {
            0
        } else {
            if padded.height >= 7 {
                footer.lines().count().clamp(1, 2) as u16
            } else {
                1
            }
            .min(padded.height)
        };
        let divider = u16::from(padded.width > 0 && padded.height >= footer_rows + 2);
        let remaining = padded.height.saturating_sub(footer_rows + divider);
        let header_rows = if padded.width > 0 && !context.is_empty() && remaining >= 4 {
            2
        } else {
            0
        };
        Self {
            outer,
            padded,
            footer_rows,
            divider,
            header_rows,
        }
    }
    fn body(&self) -> Rect {
        Rect::new(
            self.padded.x,
            self.padded.y + self.header_rows,
            self.padded.width,
            self.padded
                .height
                .saturating_sub(self.footer_rows + self.divider + self.header_rows),
        )
    }
}
/// The same pure geometry is usable before drawing, including after Resize.
pub(super) fn body_area(area: Rect, context: &str, footer: &str, height: Option<u16>) -> Rect {
    PanelLayout::new(area, context, footer, height).body()
}
fn panel_height(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    context: &str,
    footer: &str,
    height: Option<u16>,
) -> Rect {
    let layout = PanelLayout::new(area, context, footer, height);
    let body = layout.body();
    let PanelLayout {
        outer,
        padded,
        footer_rows,
        divider,
        header_rows,
    } = layout;
    // Clear full rows so a wide glyph at a popup edge cannot overwrite a border.
    frame.render_widget(
        Clear,
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        ),
    );
    frame.render_widget(Clear, outer);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::MUTED))
            .title(
                Line::from(format!(" {title} ")).style(
                    Style::default()
                        .fg(theme::EMPHASIS)
                        .add_modifier(Modifier::BOLD),
                ),
            ),
        outer,
    );
    if padded.height == 0 || padded.width == 0 {
        return padded;
    }
    let footer_y = padded.bottom().saturating_sub(footer_rows);
    if divider > 0 {
        separator(frame, Rect::new(padded.x, footer_y - 1, padded.width, 1));
    }
    frame.render_widget(
        Paragraph::new(footer.to_owned()).style(Style::default().fg(theme::TEXT)),
        Rect::new(padded.x, footer_y, padded.width, footer_rows),
    );
    if header_rows > 0 {
        frame.render_widget(
            Paragraph::new(context.to_owned()).style(Style::default().fg(theme::TEXT)),
            Rect::new(padded.x, padded.y, padded.width, 1),
        );
        separator(frame, Rect::new(padded.x, padded.y + 1, padded.width, 1));
    }
    body
}
pub(super) fn separator(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new("─".repeat(usize::from(area.width)))
            .style(Style::default().fg(theme::MUTED)),
        area,
    );
}
/// Ratatui owns item visibility and selection; rows need not have equal heights.
pub(super) fn list(frame: &mut Frame<'_>, area: Rect, items: Vec<Item>, selected: usize) -> usize {
    if area.width == 0 || area.height == 0 {
        return 0;
    }
    let count = items.len();
    let heights = items
        .iter()
        .map(|item| item.lines.len().max(1).min(usize::from(area.height)))
        .collect::<Vec<_>>();
    let total_rows = heights.iter().sum::<usize>();
    let mut state = ListState::default()
        .with_selected((count > 0).then_some(selected.min(count.saturating_sub(1))));
    let content = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };
    frame.render_stateful_widget(
        List::new(
            items
                .into_iter()
                .map(|item| item.bounded(usize::from(area.height))),
        )
        .style(normal_style())
        .highlight_style(selected_style())
        .highlight_symbol("› "),
        content,
        &mut state,
    );
    if total_rows > usize::from(area.height) {
        scrollbar(
            frame,
            area,
            total_rows,
            heights[..state.offset().min(heights.len())].iter().sum(),
        );
    }
    state.offset()
}
pub(super) fn item(text: &str, width: u16) -> Item {
    Item::new(super::presentation::hanging_wrap(
        text.lines()
            .map(|line| Line::from(line.to_owned()))
            .collect(),
        width.saturating_sub(3).max(1),
    ))
    .style(normal_style())
}
pub(super) fn reader_width(area: Rect) -> u16 {
    area.width.saturating_sub(1)
}
pub(super) fn reader(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[Line<'static>],
    requested: u16,
) -> u16 {
    if area.width == 0 || area.height == 0 {
        return 0;
    }
    let mut display = super::presentation::borrowed(lines);
    for line in &mut display {
        crate::presentation::expand_tabs(line);
    }
    reader_prepared(frame, area, display, requested)
}

/// Consume already prepared display lines without scanning source text again.
pub(super) fn reader_prepared(
    frame: &mut Frame<'_>,
    area: Rect,
    display: Vec<Line<'_>>,
    requested: u16,
) -> u16 {
    if area.width == 0 || area.height == 0 {
        return 0;
    }
    let content = Rect {
        width: reader_width(area),
        ..area
    };
    let paragraph = Paragraph::new(display).wrap(Wrap { trim: false });
    let total = paragraph.line_count(content.width.max(1));
    let scroll = usize::from(requested)
        .min(total.saturating_sub(usize::from(area.height)))
        .min(u16::MAX as usize) as u16;
    frame.render_widget(paragraph.scroll((scroll, 0)).style(normal_style()), content);
    if total > usize::from(area.height) {
        scrollbar(frame, area, total, usize::from(scroll));
    }
    scroll
}
fn scrollbar(frame: &mut Frame<'_>, area: Rect, total: usize, position: usize) {
    // This widget uses content_length - 1 as its maximum top-row position,
    // then adds the viewport length when computing the thumb proportions.
    let max_position = total.saturating_sub(usize::from(area.height));
    let mut state = ScrollbarState::new(max_position.saturating_add(1))
        .position(position.min(max_position))
        .viewport_content_length(usize::from(area.height));
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(Style::default().fg(theme::ACCENT))
            .track_style(Style::default().fg(theme::MUTED)),
        area,
        &mut state,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn list_focus_and_footer_remain_distinct_at_small_and_normal_sizes() {
        for (width, height) in [(120, 40), (80, 24), (24, 10), (8, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    let body = panel(
                        frame,
                        frame.area(),
                        "選択一覧",
                        "30件 · 対象",
                        "Esc 戻る\n↑↓ 選択 · Enter 詳細",
                    );
                    assert!(body.right() <= width && body.bottom() <= height);
                    list(
                        frame,
                        body,
                        (0..30).map(|i| Item::new(format!("item {i:02}"))).collect(),
                        29,
                    );
                })
                .unwrap();
            let text = (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("Esc"));
            if height >= 10 {
                assert!(text.contains("item 29"));
            }
            if width >= 80 {
                let buffer = terminal.backend().buffer();
                let y = (0..height)
                    .find(|y| (0..width).any(|x| buffer[(x, *y)].symbol() == "›"))
                    .unwrap();
                assert_eq!(
                    buffer[(width / 2, y)].bg,
                    selected_style().bg.unwrap(),
                    "selection fills the row"
                );
            }
        }
    }
    #[test]
    fn reader_end_and_scrollbar_end_agree_at_small_and_large_viewports() {
        for (width, height) in [(80, 24), (120, 40), (180, 65)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let lines = (0..100)
                .map(|index| Line::from(format!("row {index:03}")))
                .collect::<Vec<_>>();
            for (requested, at_end) in [(u16::MAX, true), (0, false)] {
                terminal
                    .draw(|frame| {
                        let area = frame.area();
                        let scroll = reader(frame, area, &lines, requested);
                        assert_eq!(
                            usize::from(scroll),
                            if at_end { 100 - usize::from(height) } else { 0 }
                        );
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let thumb_y = if at_end { height - 1 } else { 0 };
                assert_eq!(
                    buffer[(width - 1, thumb_y)].fg,
                    Color::Cyan,
                    "thumb must reach {} at {width}x{height}",
                    if at_end { "bottom" } else { "top" }
                );
                if at_end {
                    let last = (0..7)
                        .map(|x| buffer[(x, height - 1)].symbol())
                        .collect::<String>();
                    assert_eq!(last, "row 099");
                }
            }
        }
    }
    #[test]
    fn reader_clamps_without_overwriting_fixed_controls() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                let body = panel(
                    frame,
                    frame.area(),
                    "原文",
                    "保存済み",
                    "Esc 戻る · C コピー",
                );
                let lines = (0..100)
                    .map(|i| Line::from(format!("line {i:03}")))
                    .collect::<Vec<_>>();
                let scroll = reader(frame, body, &lines, u16::MAX);
                assert!(scroll < 100);
            })
            .unwrap();
        let text = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("line 099") && text.contains("Esc"));
    }
    #[test]
    fn reader_tabs_use_unicode_display_columns() {
        let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
        let lines = vec![Line::from(vec![
            Span::raw("日"),
            Span::styled("\tX", Style::default().fg(theme::ATTENTION)),
        ])];
        terminal
            .draw(|frame| {
                reader(frame, frame.area(), &lines, 0);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(4, 0)].symbol(), "X");
        assert_eq!(buffer[(4, 0)].fg, theme::ATTENTION);
        assert_eq!(lines[0].spans[1].content, "\tX");
    }
}
