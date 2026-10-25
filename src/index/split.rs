//! Unicode paragraph segmentation; independent of storage and HTTP.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
    pub text: String,
}
/// Unicode character spans; newline/paragraph boundaries preferred, no overlap.
pub fn split_text(text: &str, max_chars: usize, base: usize) -> Vec<Segment> {
    let max_chars = max_chars.max(1);
    let mut offset = 0;
    let mut chars = base;
    let mut segments = Vec::new();
    while offset < text.len() {
        let remaining = &text[offset..];
        let end = split_boundary(remaining, max_chars);
        let body = &remaining[..end];
        let count = body.chars().count();
        if !body.trim().is_empty() {
            segments.push(Segment {
                start: chars,
                end: chars + count,
                text: body.into(),
            });
        }
        chars += count;
        offset += end;
    }
    segments
}
fn split_boundary(text: &str, max_chars: usize) -> usize {
    let hard = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(i, _)| i);
    if hard == text.len() {
        return hard;
    }
    let prefix = &text[..hard];
    prefix
        .rfind("\n\n")
        .map(|i| i + 2)
        .or_else(|| prefix.rfind('\n').map(|i| i + 1))
        .filter(|end| *end > 0)
        .unwrap_or(hard)
}
pub(super) fn drain_segments(
    carry: &mut String,
    max_chars: usize,
    offset: &mut usize,
    done: bool,
) -> Vec<Segment> {
    let mut segments = Vec::new();
    loop {
        if carry.is_empty() || (!done && carry.chars().take(max_chars + 1).count() <= max_chars) {
            break;
        }
        let end = split_boundary(carry, max_chars.max(1));
        let body = &carry[..end];
        let count = body.chars().count();
        if !body.trim().is_empty() {
            segments.push(Segment {
                start: *offset,
                end: *offset + count,
                text: body.into(),
            });
        }
        *offset += count;
        carry.drain(..end);
    }
    segments
}
