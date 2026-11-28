//! A single popup's budgeted Ratatui raster. Raw sources stay with owners.
use super::*;
use ratatui::{
    buffer::{Buffer, Cell},
    widgets::Widget,
};
use std::mem::size_of;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Revision(u64),
    Text([u8; 32]),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct ReaderBounds {
    pub total: usize,
    pub visible: u16,
    pub maximum: u16,
}
pub(in crate::ui) struct ReaderCache {
    budget: usize,
    pub touched: Option<std::time::Instant>,
    key: Option<(Source, u16, Option<u8>)>,
    raster: Option<Buffer>,
    bytes: usize,
    bounds: ReaderBounds,
    #[cfg(test)]
    layout_builds: usize,
}
impl Default for ReaderCache {
    fn default() -> Self {
        Self {
            budget: crate::config::Cache::default().layout_bytes as usize,
            touched: None,
            key: None,
            raster: None,
            bytes: 0,
            bounds: ReaderBounds::default(),
            #[cfg(test)]
            layout_builds: 0,
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
        self.raster = None;
        self.bytes = 0;
        self.bounds = ReaderBounds::default();
    }
    pub fn reset_bounds(&mut self) {
        self.bounds = ReaderBounds::default();
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
    #[cfg(test)]
    pub fn layout_builds(&self) -> usize {
        self.layout_builds
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
        self.render_source(frame, area, Source::Revision(revision), requested, prepare)
    }

    pub fn render_text(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        digest: [u8; 32],
        requested: u16,
        prepare: impl FnOnce() -> Vec<Line<'static>>,
    ) -> u16 {
        self.render_source(frame, area, Source::Text(digest), requested, prepare)
    }

    fn render_source(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        source: Source,
        requested: u16,
        prepare: impl FnOnce() -> Vec<Line<'static>>,
    ) -> u16 {
        if area.width == 0 || area.height == 0 {
            self.bounds = ReaderBounds::default();
            return 0;
        }
        if self.touch(source, reader_width(area)) {
            return self.paint(frame, area, requested);
        }
        self.paint_source(frame, area, source, requested, &prepare())
    }

    fn touch(&mut self, revision: Source, width: u16) -> bool {
        self.touched = Some(std::time::Instant::now());
        self.key == Some(Self::key(revision, width))
    }

    fn key(revision: Source, width: u16) -> (Source, u16, Option<u8>) {
        (
            revision,
            width,
            crate::ui::text_width::terminal_width_observation(),
        )
    }

    pub fn row_count(&self, revision: u64, width: u16, lines: &[Line<'static>]) -> usize {
        if self.key == Some(Self::key(Source::Revision(revision), width))
            && let Some(raster) = &self.raster
        {
            return usize::from(raster.area.height);
        }
        prepared(lines).line_count(width.max(1))
    }

    fn paint_source(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        source: Source,
        requested: u16,
        lines: &[Line<'static>],
    ) -> u16 {
        let width = reader_width(area);
        self.key = None;
        self.raster = None;
        self.bytes = 0;
        #[cfg(test)]
        {
            self.layout_builds += 1;
        }
        let paragraph = prepared(lines).style(normal_style());
        let total = paragraph.line_count(width.max(1));
        let cell_bytes = usize::from(width)
            .saturating_mul(total)
            .saturating_mul(size_of::<Cell>());
        // Cache the actual Ratatui output, not lines reconstructed from cells.
        // Declining retention uses the same paragraph in a viewport-sized buffer.
        if width != 0 && total != 0 && total <= usize::from(u16::MAX) && cell_bytes <= self.budget {
            let extent = Rect::new(0, 0, width, total as u16);
            let mut raster = Buffer::empty(extent);
            (&paragraph).render(extent, &mut raster);
            let bytes = raster.content.iter().fold(cell_bytes, |total, cell| {
                total.saturating_add(cell.symbol().len().saturating_mul(2))
            });
            if bytes <= self.budget {
                self.key = Some(Self::key(source, width));
                self.raster = Some(raster);
                self.bytes = bytes;
                return self.paint(frame, area, requested);
            }
        }
        let scroll = self.set_bounds(total, area.height, requested);
        let extent = Rect::new(0, 0, width, area.height);
        let mut viewport = Buffer::empty(extent);
        paragraph.scroll((scroll, 0)).render(extent, &mut viewport);
        copy_viewport(frame, area, &viewport, 0);
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
        let total = usize::from(self.raster.as_ref().expect("hit has a raster").area.height);
        let scroll = self.set_bounds(total, area.height, requested);
        let raster = self.raster.as_ref().expect("hit has a raster");
        copy_viewport(frame, area, raster, scroll);
        if total > usize::from(area.height) {
            scrollbar(frame, area, total, usize::from(scroll));
        }
        scroll
    }
}
fn copy_viewport(frame: &mut Frame<'_>, area: Rect, raster: &Buffer, start: u16) {
    let extent = area.intersection(*frame.buffer_mut().area());
    let mut blank = Cell::EMPTY;
    blank.set_style(normal_style());
    for y in extent.top()..extent.bottom() {
        let row = start.saturating_add(y - area.top());
        for x in extent.left()
            ..area
                .left()
                .saturating_add(raster.area.width)
                .min(extent.right())
        {
            let cell = if row < raster.area.height {
                &raster[(x - area.left(), row)]
            } else {
                &blank
            };
            frame.buffer_mut()[(x, y)].clone_from(cell);
        }
    }
}
fn prepared<'a>(lines: &'a [Line<'static>]) -> Paragraph<'a> {
    let mut display = super::super::presentation::borrowed(lines);
    for line in &mut display {
        crate::presentation::expand_tabs(line);
    }
    Paragraph::new(display).wrap(Wrap { trim: false })
}
pub(in crate::ui) fn reader_cached(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[Line<'static>],
    requested: u16,
    cache: &mut ReaderCache,
    revision: u64,
) -> u16 {
    if area.width == 0 || area.height == 0 {
        cache.bounds = ReaderBounds::default();
        return 0;
    }
    let source = Source::Revision(revision);
    if cache.touch(source, reader_width(area)) {
        cache.paint(frame, area, requested)
    } else {
        cache.paint_source(frame, area, source, requested, lines)
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
                Span::styled("  ", Style::default().bg(theme::ATTENTION)),
            ]),
            Line::from(""),
            Line::from("last row"),
        ];
        for width in [8, 20, 80] {
            for requested in [0, 1, 3, u16::MAX] {
                let mut cache = ReaderCache::default();
                let (cached, position) = paint(&mut cache, &lines, width, 4, 1, requested);
                let (hit, hit_position) = paint(&mut cache, &lines, width, 4, 1, requested);
                assert_eq!((hit, hit_position), (cached.clone(), position));
                let mut uncached = ReaderCache::default();
                uncached.set_budget(0);
                let (miss, miss_position) = paint(&mut uncached, &lines, width, 4, 1, requested);
                assert_eq!((miss, miss_position), (cached.clone(), position));
                assert_eq!(uncached.bounds(), cache.bounds());
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
