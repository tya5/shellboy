//! Fixed-region geometry and row presentation, without domain state or I/O.
use super::*;
use crate::ui::text_width::glyphs;

/// Immutable configuration shared by independently focused UI regions.
/// Domain owners retain selected identities, snapshots, clocks and actions.
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
