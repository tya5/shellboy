//! A single popup's finite, styled Ratatui-wrap result. Raw sources stay with owners.
use super::*;
use std::mem::size_of;
const MAX_ROWS: usize = 65_536;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 256 * 1024;
const LIMIT_NOTICE: &str = "表示キャッシュ上限 · 原文のコピー・取得元から全文を確認";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct ReaderBounds {
    pub total: usize,
    pub visible: u16,
    pub maximum: u16,
}
#[derive(Default)]
pub(in crate::ui) struct ReaderCache {
    key: Option<(u64, u16)>,
    rows: Vec<Line<'static>>,
    bytes: usize,
    bounds: ReaderBounds,
    limited: bool,
    #[cfg(test)]
    builds: usize,
}
impl ReaderCache {
    pub fn clear(&mut self) {
        self.key = None;
        self.rows = Vec::new();
        self.bytes = 0;
        self.bounds = ReaderBounds::default();
        self.limited = false;
    }
    #[cfg(test)]
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
    #[cfg(test)]
    pub(in crate::ui) fn build_count(&self) -> usize {
        self.builds
    }
    pub fn bounds(&self) -> ReaderBounds {
        self.bounds
    }
    #[cfg(test)]
    pub fn limited(&self) -> bool {
        self.limited
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
        self.prepare(revision, width, prepare);
        self.paint(frame, area, requested)
    }
    /// Prepare once for source revision/width; geometry can use this row count
    /// without rewrapping the source on every keyboard or wheel operation.
    pub fn prepare(
        &mut self,
        revision: u64,
        width: u16,
        make_lines: impl FnOnce() -> Vec<Line<'static>>,
    ) -> usize {
        let width = width.max(1);
        if self.key != Some((revision, width)) {
            self.rebuild(revision, width, &make_lines());
        }
        self.rows.len()
    }
    fn rebuild(&mut self, revision: u64, width: u16, lines: &[Line<'static>]) {
        self.rows = Vec::new();
        self.bytes = 0;
        self.limited = false;
        self.key = Some((revision, width));
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let mut source_bytes = 0usize;
        for source in lines {
            if self.rows.len() >= MAX_ROWS - 1 || source_bytes >= MAX_SOURCE_BYTES {
                self.limited = true;
                break;
            }
            // Keep the existing OSS wrapper. Stop source copying before either
            // the row limit (including expanded tabs) or byte budget can explode.
            let mut line = Line::default().style(source.style);
            line.alignment = source.alignment;
            let mut remaining = MAX_SOURCE_BYTES - source_bytes;
            let row_floor = 2 * size_of::<Line<'static>>() + size_of::<Span<'static>>() + 4;
            let affordable_rows = (MAX_BYTES.saturating_sub(self.bytes + 4096) / row_floor)
                .min(MAX_ROWS - 1 - self.rows.len());
            if affordable_rows == 0 {
                self.limited = true;
                break;
            }
            remaining = remaining.min(affordable_rows.saturating_mul(usize::from(width)));
            let mut copied = 0usize;
            for span in &source.spans {
                if copied >= remaining
                    || (line.spans.len() + 1).saturating_mul(size_of::<Span<'static>>())
                        >= MAX_BYTES / 4
                {
                    self.limited = true;
                    break;
                }
                let mut end = (remaining - copied).min(span.content.len());
                while !span.content.is_char_boundary(end) {
                    end -= 1;
                }
                line.spans
                    .push(Span::styled(span.content[..end].to_owned(), span.style));
                copied += end;
                if end < span.content.len() {
                    self.limited = true;
                    break;
                }
            }
            crate::presentation::expand_tabs(&mut line);
            // A tab may expand an otherwise small source. Bound it before wrapping.
            let expanded = line
                .spans
                .iter()
                .map(|span| span.content.len())
                .sum::<usize>();
            if expanded > remaining {
                let mut available = remaining;
                for span in &mut line.spans {
                    let mut end = available.min(span.content.len());
                    while !span.content.is_char_boundary(end) {
                        end -= 1;
                    }
                    span.content = span.content[..end].to_owned().into();
                    available -= end;
                }
                self.limited = true;
            }
            source_bytes = source_bytes.saturating_add(copied.max(1));
            for row in super::super::presentation::hanging_wrap(vec![line], width) {
                let charge = row_bytes(&row);
                if self.rows.len() >= MAX_ROWS - 1
                    || self.bytes.saturating_add(charge) > MAX_BYTES - 4096
                {
                    self.limited = true;
                    break;
                }
                self.bytes += charge;
                self.rows.push(row);
            }
            if self.limited {
                break;
            }
        }
        if self.limited {
            let notice = Line::from(LIMIT_NOTICE).style(Style::default().fg(theme::WARNING));
            self.bytes += row_bytes(&notice);
            self.rows.push(notice);
        }
    }
    fn paint(&mut self, frame: &mut Frame<'_>, area: Rect, requested: u16) -> u16 {
        let total = self.rows.len();
        let maximum = total
            .saturating_sub(usize::from(area.height))
            .min(u16::MAX as usize) as u16;
        self.bounds = ReaderBounds {
            total,
            visible: area.height,
            maximum,
        };
        let scroll = requested.min(maximum);
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
    if cache.key != Some((revision, width)) {
        cache.rebuild(revision, width, lines);
    }
    cache.paint(frame, area, requested)
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
    fn sixty_kib_original_wraps_once_across_sixty_wheels_and_only_page_or_width_rebuilds() {
        let original = "line of saved original text 0123456789\n".repeat(1700);
        assert!(original.len() >= 64 * 1024);
        let lines = original
            .lines()
            .map(|line| Line::from(line.to_owned()))
            .collect::<Vec<_>>();
        let mut cache = ReaderCache::default();
        for event in 0..60 {
            paint(&mut cache, &lines, 80, 24, 11, event * 3);
        }
        assert_eq!(cache.builds, 1);
        assert!(!cache.limited());
        assert!(cache.retained_bytes() <= MAX_BYTES);
        assert!(cache.bounds().maximum > 1000);
        paint(&mut cache, &lines, 140, 24, 11, 0);
        assert_eq!(cache.builds, 2);
        paint(&mut cache, &[Line::from("different page")], 140, 24, 12, 0);
        assert_eq!(cache.builds, 3);
        assert_eq!(cache.bounds().total, 1);
        cache.clear();
        assert_eq!(cache.retained_bytes(), 0);
        assert_eq!(cache.bounds(), ReaderBounds::default());
    }
    #[test]
    fn lazy_prepare_is_not_called_on_scroll_hit_and_narrow_limit_is_explicit() {
        let mut cache = ReaderCache::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        let mut prepares = 0;
        for scroll in 0..60 {
            terminal
                .draw(|frame| {
                    cache.render(frame, frame.area(), 9, scroll, || {
                        prepares += 1;
                        vec![Line::from("saved ".repeat(10_000))]
                    });
                })
                .unwrap();
        }
        assert_eq!(prepares, 1);
        let mut lines = vec![Line::from("x".repeat(300_000))];
        let original_bytes = lines[0].spans[0].content.len();
        paint(&mut cache, &lines, 2, 8, 10, u16::MAX);
        assert!(cache.limited());
        assert!(cache.rows.len() <= MAX_ROWS);
        assert!(cache.retained_bytes() <= MAX_BYTES);
        assert!(
            cache
                .rows
                .last()
                .unwrap()
                .spans
                .iter()
                .any(|span| span.content.contains("表示キャッシュ上限"))
        );
        assert_eq!(
            lines[0].spans[0].content.len(),
            original_bytes,
            "copy source is independent of display cache"
        );
        lines.clear();
    }
}
