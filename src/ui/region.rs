//! Fixed-region geometry and row presentation, without domain state or I/O.
use super::*;
use crate::ui::text_width::glyphs;
pub(super) use glyphs::numbered_branch;
use ratatui::style::Modifier;

/// Immutable configuration shared by independently focused UI regions.
/// Domain owners retain payloads, snapshots, clocks and actions.
#[derive(Clone, Copy, Debug)]
pub(super) struct RegionLayout {
    capacity: usize,
    collapsed: bool,
}

impl RegionLayout {
    pub fn new(capacity: usize, collapsed: bool) -> Self {
        Self {
            capacity,
            collapsed,
        }
    }

    pub fn height(self, available: u16, preview: u16) -> u16 {
        if available <= 3 {
            return 0;
        }
        let requested = if self.collapsed {
            2
        } else {
            2 + self.capacity as u16 + preview
        };
        requested.min(available.saturating_sub(3))
    }

    pub fn visible_rows(self, area: Rect, total: usize, overflow: bool) -> usize {
        if self.collapsed {
            return 0;
        }
        let body = usize::from(area.height.saturating_sub(2)).min(self.capacity);
        let slots = if overflow && body >= 2 {
            body - 1
        } else {
            body
        };
        total.min(slots)
    }

    pub fn pad_body(self, area: Rect, lines: &mut Vec<Line<'_>>) {
        if !self.collapsed {
            let body = 1 + usize::from(area.height > 1) + self.capacity;
            lines.resize_with(body.min(usize::from(area.height)).max(lines.len()), || {
                Line::from("")
            });
        }
    }
}

/// Common marker and clipping; the domain supplies localized heading facts.
pub(super) fn heading(width: u16, text: &str, palette: theme::Palette, dim: bool) -> Line<'static> {
    let mark = glyphs::HEADING;
    let heading = fit(&format!("{mark} {text}"), usize::from(width));
    let dim = dim
        .then(|| palette.live_work_dim())
        .flatten()
        .filter(|_| heading.starts_with(mark));
    match dim {
        Some(dim) => Line::from(vec![
            Span::styled(mark.to_owned(), dim),
            Span::styled(heading[mark.len()..].to_owned(), palette.live_work()),
        ]),
        None => Line::styled(heading, palette.live_work()),
    }
}

pub(super) fn heading_lines(area: Rect, heading: Line<'_>) -> Vec<Line<'_>> {
    if area.height > 1 {
        vec![Line::from(""), heading]
    } else {
        vec![heading]
    }
}

pub(super) fn fit(text: &str, width: usize) -> String {
    use crate::ui::text_width::UnicodeWidthStr;
    use unicode_segmentation::UnicodeSegmentation;
    if text.width() <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for part in text.graphemes(true) {
        if used + part.width() + 1 > width {
            break;
        }
        used += part.width();
        result.push_str(part);
    }
    result.push_str(glyphs::ellipsis());
    result
}

/// Allocate the existing right-side budget for region and notice rows.
pub(super) fn row_columns(
    width: usize,
    prefix: &str,
    label: &str,
    metadata: &str,
    overflow: &str,
) -> (String, usize, String) {
    use crate::ui::text_width::UnicodeWidthStr;
    let right_budget = width / 2;
    let overflow = fit(overflow, right_budget);
    let right = format!(
        "{}{overflow}",
        fit(metadata, right_budget.saturating_sub(overflow.width()))
    );
    let label = fit(
        label,
        width.saturating_sub(prefix.width() + right.width() + 3),
    );
    let gap = width.saturating_sub(prefix.width() + label.width() + right.width());
    (label, gap, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_geometry_matches_original_bounds_and_overflow_slots() {
        for capacity in 1..=10 {
            for collapsed in [false, true] {
                let layout = RegionLayout::new(capacity, collapsed);
                for available in 0..=20 {
                    for preview in [0, 3] {
                        let expected = if available <= 3 {
                            0
                        } else {
                            (if collapsed {
                                2
                            } else {
                                2 + capacity as u16 + preview
                            })
                            .min(available - 3)
                        };
                        assert_eq!(layout.height(available, preview), expected);
                    }
                    let area = Rect::new(3, 4, 18, available);
                    for overflow in [false, true] {
                        let body = usize::from(available.saturating_sub(2)).min(capacity);
                        let slots = body.saturating_sub(usize::from(overflow && body >= 2));
                        for total in 0..=12 {
                            assert_eq!(
                                layout.visible_rows(area, total, overflow),
                                if collapsed { 0 } else { total.min(slots) }
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn heading_and_padding_preserve_existing_lines_and_short_areas() {
        for height in 1..=10 {
            for collapsed in [false, true] {
                let area = Rect::new(0, 0, 40, height);
                let mut lines = heading_lines(area, Line::raw("heading"));
                let heading_index = usize::from(height > 1);
                assert_eq!(lines[heading_index], Line::raw("heading"));
                let initial = lines.clone();
                RegionLayout::new(3, collapsed).pad_body(area, &mut lines);
                assert_eq!(&lines[..initial.len()], &initial);
                let expected = if collapsed {
                    initial.len()
                } else {
                    (heading_index + 4).min(usize::from(height))
                };
                assert_eq!(lines.len(), expected);
                assert!(
                    lines[initial.len()..]
                        .iter()
                        .all(|line| *line == Line::raw(""))
                );
            }
        }
    }
}

/// UI-local region focus and geometry, parameterized only by stable row identity.
/// Domain payloads, clocks, requests and preview/source bodies stay with owners.
#[derive(Debug)]
pub(super) struct RegionState<K> {
    pub active: bool,
    pub selected: Option<K>,
    pub area: Option<Rect>,
    pub visible_rows: usize,
    pub displayed_keys: Vec<K>,
    pub collapsed: bool,
    capacity: usize,
}

impl<K> Default for RegionState<K> {
    fn default() -> Self {
        Self {
            active: false,
            selected: None,
            area: None,
            visible_rows: 0,
            displayed_keys: Vec::new(),
            collapsed: false,
            capacity: 0,
        }
    }
}

impl<K> RegionState<K> {
    pub fn cap(&self) -> usize {
        if self.capacity == 0 {
            10
        } else {
            self.capacity
        }
    }

    pub fn configure(&mut self, collapsed: bool, capacity: usize) {
        self.collapsed = collapsed;
        self.capacity = capacity.clamp(1, 10);
        if collapsed {
            self.active = false;
        }
    }

    pub fn layout(&self) -> RegionLayout {
        RegionLayout::new(self.cap(), self.collapsed)
    }

    pub fn set_area(&mut self, area: Rect, total: usize, overflow: bool) {
        self.area = (area.height > 0).then_some(area);
        self.visible_rows = self.layout().visible_rows(area, total, overflow);
        if area.height == 0 {
            self.active = false;
        }
    }

    pub fn hidden(&self, total: usize, overflow: u64) -> u64 {
        overflow.saturating_add((total - self.visible_rows) as u64)
    }

    pub fn hit(&self, column: u16, row: u16) -> RegionHit {
        let Some(area) = self.area else {
            return RegionHit::Outside;
        };
        if !area.contains(ratatui::layout::Position::new(column, row)) {
            return RegionHit::Outside;
        }
        if row == area.y + u16::from(area.height > 1) {
            return RegionHit::Heading;
        }
        let Some(offset) = row.saturating_sub(area.y).checked_sub(2) else {
            return RegionHit::Blank;
        };
        if usize::from(offset) < self.visible_rows {
            RegionHit::Row(usize::from(offset))
        } else {
            RegionHit::Blank
        }
    }
}

impl<K: PartialEq> RegionState<K> {
    pub fn next_index<'a>(&self, mut keys: impl Iterator<Item = &'a K>, down: bool) -> usize
    where
        K: 'a,
    {
        let index = self
            .selected
            .as_ref()
            .and_then(|selected| keys.position(|key| key == selected))
            .unwrap_or(0);
        if down {
            (index + 1).min(self.visible_rows.saturating_sub(1))
        } else {
            index.saturating_sub(1)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RegionHit {
    Outside,
    Heading,
    Row(usize),
    Blank,
}

#[cfg(test)]
mod state_tests {
    use super::*;
    // Fold and focus remain structural: no caret/reverse-video behavior changes.
    #[test]
    fn region_identity_navigation_and_hits_preserve_existing_bounds() {
        let mut state = RegionState::<String>::default();
        state.configure(false, 3);
        state.set_area(Rect::new(4, 5, 30, 5), 4, true);
        let keys = [
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
        ];
        assert_eq!(state.visible_rows, 2);
        assert_eq!(state.hidden(keys.len(), 3), 5);
        state.selected = Some(keys[0].clone());
        assert_eq!(state.next_index(keys.iter(), false), 0);
        assert_eq!(state.next_index(keys.iter(), true), 1);
        state.selected = Some(keys[1].clone());
        assert_eq!(state.next_index(keys.iter(), true), 1);
        assert_eq!(state.hit(4, 5), RegionHit::Blank);
        assert_eq!(state.hit(4, 6), RegionHit::Heading);
        assert_eq!(state.hit(4, 7), RegionHit::Row(0));
        assert_eq!(state.hit(4, 8), RegionHit::Row(1));
        assert_eq!(state.hit(4, 9), RegionHit::Blank);
        assert_eq!(state.hit(3, 6), RegionHit::Outside);
        state.active = true;
        state.configure(true, 3);
        state.set_area(Rect::new(4, 5, 30, 2), 4, true);
        assert!(!state.active);
        assert_eq!(state.visible_rows, 0);
        assert_eq!(state.selected.as_ref(), Some(&keys[1]));
        assert_eq!(state.hit(4, 6), RegionHit::Heading);
        state.configure(false, 3);
        state.set_area(Rect::new(4, 5, 30, 0), 4, true);
        assert_eq!(state.hit(4, 5), RegionHit::Outside);
    }
}

/// Focus is visible even in a no-color terminal, without adding a caret glyph.
pub(super) fn selection_style(selected: bool) -> Style {
    if selected {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// Domain-provided facts; clipping, alignment, palette and focus are shared.
pub(super) struct RegionRow<'a> {
    pub prefix: String,
    pub label: &'a str,
    pub metadata: &'a str,
    pub overflow: &'a str,
    pub selected: bool,
    pub metadata_style: Style,
}
impl RegionRow<'_> {
    pub fn paint(self, width: u16, palette: theme::Palette) -> Line<'static> {
        let (label, gap, right) = row_columns(
            usize::from(width),
            &self.prefix,
            self.label,
            self.metadata,
            self.overflow,
        );
        Line::from(vec![
            Span::styled(self.prefix, palette.live_work()),
            Span::styled(label, palette.live_work_content()),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, self.metadata_style),
        ])
        .style(selection_style(self.selected))
    }
}
