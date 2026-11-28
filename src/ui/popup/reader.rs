//! A single popup's finite, styled Ratatui-wrap result. Raw sources stay with owners.
use super::*;
use std::mem::size_of;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct ReaderBounds {
    pub total: usize,
    pub visible: u16,
    pub maximum: u16,
}
pub(in crate::ui) struct ReaderCache {
    budget: usize,
    pub touched: Option<std::time::Instant>,
    key: Option<(u64, u16, Option<u8>)>,
    rows: Vec<Line<'static>>,
    bytes: usize,
    bounds: ReaderBounds,
}
impl Default for ReaderCache {
    fn default() -> Self {
        Self {
            budget: crate::config::Cache::default().layout_bytes as usize,
            touched: None,
            key: None,
            rows: Vec::new(),
            bytes: 0,
            bounds: ReaderBounds::default(),
        }
    }
}
impl ReaderCache {
    pub fn set_budget(&mut self, budget: usize) {
        if self.budget != budget {
            self.budget = budget;
            self.clear();
        }
    }
    pub fn clear(&mut self) {
        self.key = None;
        self.touched = None;
        self.rows = Vec::new();
        self.bytes = 0;
        self.bounds = ReaderBounds::default();
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
    pub fn bounds(&self) -> ReaderBounds {
        self.bounds
    }
    /// `revision` belongs to this popup's content/page. Preparing is lazy on a hit.
    pub fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        revision: u64,
        requested: u16,
        prepare: impl FnOnce() -> Vec<Line<'static>>,
    ) -> u16 {
        let width = reader_width(area).max(1);
        if area.width == 0 || area.height == 0 {
            self.bounds = ReaderBounds::default();
            return 0;
        }
        if self.touch(revision, width) {
            return self.paint(frame, area, requested);
        }
        self.paint_source(frame, area, revision, requested, &prepare())
    }

    fn touch(&mut self, revision: u64, width: u16) -> bool {
        self.touched = Some(std::time::Instant::now());
        self.key == Some(Self::key(revision, width))
    }

    fn key(revision: u64, width: u16) -> (u64, u16, Option<u8>) {
        (
            revision,
            width,
            crate::ui::text_width::terminal_width_observation(),
        )
    }

    pub fn row_count(&self, revision: u64, width: u16, lines: &[Line<'static>]) -> usize {
        if self.key == Some(Self::key(revision, width)) {
            return self.rows.len();
        }
        prepared(lines).line_count(width)
    }

    fn paint_source(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        revision: u64,
        requested: u16,
        lines: &[Line<'static>],
    ) -> u16 {
        let width = reader_width(area).max(1);
        self.key = None;
        self.rows = Vec::new();
        self.bytes = 0;
        // Admission only controls retained rows. On a miss that cannot fit,
        // Ratatui wraps the original and paints only the requested viewport.
        let paragraph = prepared(lines);
        let total = paragraph.line_count(width);
        let row_floor = 2 * size_of::<Line<'static>>();
        if self.budget != 0 && total <= u16::MAX.into() && total <= self.budget / row_floor {
            let mut bytes = 0usize;
            let mut rows = Vec::new();
            for line in lines {
                let wrapped = super::super::presentation::hanging_wrap(vec![line.clone()], width);
                bytes = bytes.saturating_add(wrapped.iter().map(row_bytes).sum::<usize>());
                if bytes > self.budget {
                    break;
                }
                rows.extend(wrapped);
            }
            if bytes <= self.budget {
                self.key = Some(Self::key(revision, width));
                self.rows = rows;
                self.bytes = bytes;
                return self.paint(frame, area, requested);
            }
        }
        let scroll = self.set_bounds(total, area.height, requested);
        frame.render_widget(
            paragraph.scroll((scroll, 0)).style(normal_style()),
            Rect {
                width: reader_width(area),
                ..area
            },
        );
        if total > usize::from(area.height) {
            scrollbar(frame, area, total, usize::from(scroll));
        }
        scroll
    }

    fn set_bounds(&mut self, total: usize, visible: u16, requested: u16) -> u16 {
        let maximum = total
            .saturating_sub(usize::from(visible))
            .min(u16::MAX.into()) as u16;
        self.bounds = ReaderBounds {
            total,
            visible,
            maximum,
        };
        requested.min(maximum)
    }
    fn paint(&mut self, frame: &mut Frame<'_>, area: Rect, requested: u16) -> u16 {
        let total = self.rows.len();
        let scroll = self.set_bounds(total, area.height, requested);
        let start = usize::from(scroll);
        let end = (start + usize::from(area.height)).min(total);
        let display = super::super::presentation::borrowed(&self.rows[start..end]);
        frame.render_widget(
            Paragraph::new(display).style(normal_style()),
            Rect {
                width: reader_width(area),
                ..area
            },
        );
        if total > usize::from(area.height) {
            scrollbar(frame, area, total, start);
        }
        scroll
    }
}
fn prepared<'a>(lines: &'a [Line<'static>]) -> Paragraph<'a> {
    let mut display = super::super::presentation::borrowed(lines);
    for line in &mut display {
        crate::presentation::expand_tabs(line);
    }
    Paragraph::new(display).wrap(Wrap { trim: false })
}
fn row_bytes(row: &Line<'_>) -> usize {
    // Vec growth is geometric; charge twice the inline row size as well as the
    // actual span allocation and owned text capacity, rather than text alone.
    2 * size_of::<Line<'static>>()
        + row.spans.capacity() * size_of::<Span<'static>>()
        + row
            .spans
            .iter()
            .map(|span| match &span.content {
                std::borrow::Cow::Owned(text) => text.capacity(),
                std::borrow::Cow::Borrowed(_) => 0,
            })
            .sum::<usize>()
}
pub(in crate::ui) fn reader_cached(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[Line<'static>],
    requested: u16,
    cache: &mut ReaderCache,
    revision: u64,
) -> u16 {
    let width = reader_width(area).max(1);
    if area.width == 0 || area.height == 0 {
        cache.bounds = ReaderBounds::default();
        return 0;
    }
    if cache.touch(revision, width) {
        cache.paint(frame, area, requested)
    } else {
        cache.paint_source(frame, area, revision, requested, lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn paint(
        cache: &mut ReaderCache,
        lines: &[Line<'static>],
        width: u16,
        height: u16,
        revision: u64,
        scroll: u16,
    ) -> (ratatui::buffer::Buffer, u16) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut actual = 0;
        terminal
            .draw(|frame| {
                actual = reader_cached(frame, frame.area(), lines, scroll, cache, revision)
            })
            .unwrap();
        (terminal.backend().buffer().clone(), actual)
    }
    #[test]
    fn cached_styled_rows_match_existing_reader_unicode_tabs_and_end_clamping() {
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    "英語 mixed 日本語\tlong words",
                    Style::default().fg(theme::ATTENTION),
                ),
                Span::raw(" next"),
            ]),
            Line::from(""),
            Line::from("last row"),
        ];
        for width in [8, 20, 80] {
            for requested in [0, 1, 3, u16::MAX] {
                let mut cache = ReaderCache::default();
                let (cached, position) = paint(&mut cache, &lines, width, 4, 1, requested);
                let mut terminal = Terminal::new(TestBackend::new(width, 4)).unwrap();
                let mut expected = 0;
                terminal
                    .draw(|frame| {
                        expected = super::super::reader(frame, frame.area(), &lines, requested)
                    })
                    .unwrap();
                assert_eq!(position, expected, "width {width}, scroll {requested}");
                assert_eq!(
                    &cached,
                    terminal.backend().buffer(),
                    "width {width}, scroll {requested}"
                );
            }
        }
    }
    #[test]
    fn small_and_zero_budgets_still_scroll_to_original_tail() {
        let mut lines = vec![Line::from("x".repeat(120_000))];
        lines.push(Line::from("original tail"));
        for budget in [0, 1024] {
            let mut cache = ReaderCache::default();
            cache.set_budget(budget);
            let (_, first) = paint(&mut cache, &lines, 80, 12, 9, 0);
            let (last, end) = paint(&mut cache, &lines, 80, 12, 9, u16::MAX);
            assert_eq!(first, 0);
            assert!(end > first);
            let text: String = last.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("original tail"));
        }
    }
}
