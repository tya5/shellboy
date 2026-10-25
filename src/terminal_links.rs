//! OSC 8 transport around Crossterm's cell renderer. Escape sequences never enter a Cell.
use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::{Buffer, Cell, CellDiffOption},
    layout::{Position, Rect, Size},
    style::Color,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    rc::Rc,
    sync::Arc,
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRegion {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub url: Arc<str>,
}
#[derive(Clone, Debug)]
pub struct LinkSnapshot {
    pub buffer: Buffer,
    pub regions: Vec<LinkRegion>,
}
impl Default for LinkSnapshot {
    fn default() -> Self {
        Self {
            buffer: Buffer::empty(Rect::default()),
            regions: vec![],
        }
    }
}
/// Current viewport snapshot shared only by the terminal's UI thread.
/// Replace after rendering, before Terminal hands its cell diff to Backend::draw.
pub type SharedLinkSnapshot = Rc<RefCell<LinkSnapshot>>;

pub struct HyperlinkBackend<W: Write> {
    inner: CrosstermBackend<W>,
    snapshot: SharedLinkSnapshot,
    previous: BTreeMap<(u16, u16), Arc<str>>,
    invalidated: bool,
}
impl<W: Write> HyperlinkBackend<W> {
    pub fn new(writer: W, snapshot: SharedLinkSnapshot) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            snapshot,
            previous: BTreeMap::new(),
            invalidated: false,
        }
    }
    pub fn snapshot(&self) -> SharedLinkSnapshot {
        Rc::clone(&self.snapshot)
    }
}
pub fn valid_link_target(url: &str) -> bool {
    !url.chars().any(char::is_control)
        && url::Url::parse(url).is_ok_and(|parsed| {
            matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
        })
}
pub(crate) fn marker_color(index: usize) -> Color {
    let marker = (index + 1) as u32;
    Color::Rgb((marker >> 16) as u8, (marker >> 8) as u8, marker as u8)
}
fn marker_index(color: Color) -> Option<usize> {
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    ((usize::from(r) << 16) | (usize::from(g) << 8) | usize::from(b)).checked_sub(1)
}
pub(crate) fn collect_regions(
    buffer: &Buffer,
    area: Rect,
    targets: &[Arc<str>],
    marker: impl Fn(&Cell) -> Color,
) -> Vec<LinkRegion> {
    let mut regions: Vec<LinkRegion> = Vec::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let Some(index) = marker_index(marker(&buffer[(x, y)])) else {
                continue;
            };
            let Some(url) = targets.get(index) else {
                continue;
            };
            if let Some(previous) = regions.last_mut().filter(|region| {
                region.y == y && region.x + region.width == x && region.url == *url
            }) {
                previous.width += 1;
            } else {
                regions.push(LinkRegion {
                    x,
                    y,
                    width: 1,
                    url: Arc::clone(url),
                });
            }
        }
    }
    regions
}
fn width(cell: &Cell) -> u16 {
    match cell.diff_option {
        CellDiffOption::ForcedWidth(width) => width.get(),
        _ => u16::try_from(cell.symbol().width().max(1)).unwrap_or(u16::MAX),
    }
}
fn leading_cells(buffer: &Buffer) -> BTreeMap<(u16, u16), (u16, u16)> {
    let mut result = BTreeMap::new();
    let area = buffer.area;
    for y in area.y..area.bottom() {
        let mut x = area.x;
        while x < area.right() {
            let lead = (x, y);
            let w = width(&buffer[lead]).min(area.right() - x).max(1);
            for covered in x..x + w {
                result.insert((covered, y), lead);
            }
            x += w;
        }
    }
    result
}
const CLOSE: &[u8] = b"\x1b]8;;\x1b\\";
impl<W: Write> Backend for HyperlinkBackend<W> {
    type Error = io::Error;
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let snapshot = self.snapshot.borrow();
        let leads = leading_cells(&snapshot.buffer);
        let mut links = BTreeMap::new();
        for region in &snapshot.regions {
            if !valid_link_target(&region.url) {
                continue;
            }
            let area = snapshot.buffer.area;
            if region.y < area.y || region.y >= area.bottom() {
                continue;
            }
            for x in region.x.max(area.x)..region.x.saturating_add(region.width).min(area.right()) {
                if let Some(&lead) = leads.get(&(x, region.y)) {
                    links.insert(lead, Arc::clone(&region.url));
                }
            }
        }
        let mut cells: BTreeMap<(u16, u16), Cell> = BTreeMap::new();
        for (x, y, cell) in content {
            let point = leads.get(&(x, y)).copied().unwrap_or((x, y));
            let value = if snapshot.buffer.area.contains(Position::from(point)) {
                &snapshot.buffer[point]
            } else {
                cell
            };
            if value.diff_option != CellDiffOption::Skip {
                cells.insert(point, value.clone());
            }
        }
        if self.invalidated {
            for &lead in leads.values() {
                let cell = &snapshot.buffer[lead];
                if cell.diff_option != CellDiffOption::Skip {
                    cells.insert(lead, cell.clone());
                }
            }
        }
        let affected: BTreeSet<_> = self.previous.keys().chain(links.keys()).copied().collect();
        for point in affected {
            if self.previous.get(&point) == links.get(&point) {
                continue;
            }
            if let Some(&lead) = leads.get(&point) {
                let cell = &snapshot.buffer[lead];
                if cell.diff_option != CellDiffOption::Skip {
                    cells.insert(lead, cell.clone());
                }
            }
        }
        // BTreeMap's tuple order is x/y; explicitly use terminal row order.
        let mut ordered: Vec<_> = cells.iter().collect();
        ordered.sort_by_key(|((x, y), _)| (*y, *x));
        self.inner.write_all(CLOSE)?;
        let mut start = 0;
        while start < ordered.len() {
            let target = links.get(ordered[start].0);
            let mut end = start + 1;
            while end < ordered.len() && links.get(ordered[end].0) == target {
                let ((px, py), pc) = ordered[end - 1];
                let ((x, y), _) = ordered[end];
                if y != py || *x != px.saturating_add(width(pc)) {
                    break;
                }
                end += 1;
            }
            if let Some(url) = target {
                write!(self.inner, "\x1b]8;;{url}\x1b\\")?;
            }
            let drawn = self.inner.draw(
                ordered[start..end]
                    .iter()
                    .map(|((x, y), cell)| (*x, *y, *cell)),
            );
            let closed = self.inner.write_all(CLOSE);
            drawn?;
            closed?;
            start = end;
        }
        self.previous = links;
        self.invalidated = false;
        Ok(())
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.invalidated = true;
        self.inner.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.invalidated = true;
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.invalidated = true;
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}
impl<W: Write> Write for HyperlinkBackend<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)
    }
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    #[test]
    fn unassigned_and_out_of_range_colors_are_not_click_targets() {
        let area = Rect::new(3, 4, 5, 2);
        let mut buffer = Buffer::empty(area);
        let targets = [Arc::<str>::from("https://example.invalid/complete")];
        buffer[(3, 4)].fg = marker_color(0);
        buffer[(4, 4)].fg = marker_color(0);
        buffer[(5, 4)].fg = Color::Rgb(0, 0, 0);
        buffer[(6, 4)].fg = marker_color(1);
        buffer[(7, 4)].fg = marker_color(0);
        buffer[(3, 5)].fg = marker_color(0);
        let regions = collect_regions(&buffer, area, &targets, |cell| cell.fg);
        assert_eq!(
            regions,
            vec![
                LinkRegion {
                    x: 3,
                    y: 4,
                    width: 2,
                    url: Arc::clone(&targets[0])
                },
                LinkRegion {
                    x: 7,
                    y: 4,
                    width: 1,
                    url: Arc::clone(&targets[0])
                },
                LinkRegion {
                    x: 3,
                    y: 5,
                    width: 1,
                    url: Arc::clone(&targets[0])
                },
            ]
        );
    }
}
