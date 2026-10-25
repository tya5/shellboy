//! Metadata for the pinned OSS renderer's link style callbacks, never Markdown layout.
use super::{PresentationLink, markdown_options};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::style::Color;
use std::sync::atomic::{AtomicUsize, Ordering};

enum CodeMarker {
    Plain,
    Unavailable,
    Link(usize),
}

pub(super) struct LinkMarkers {
    calls: AtomicUsize,
    expected: usize,
    code_calls: AtomicUsize,
    expected_codes: usize,
    codes: Vec<CodeMarker>,
    markers: Vec<(usize, usize)>,
}
impl LinkMarkers {
    pub(super) fn new(text: &str, links: &[PresentationLink]) -> Self {
        let mut expected = 0;
        let mut markers = Vec::new();
        let mut active = None;
        let mut stack = Vec::new();
        let mut depth = 0usize;
        let mut codes = Vec::new();
        let mut expected_codes = 0;
        for event in Parser::new_ext(text, markdown_options()) {
            // tui-markdown 0.3.12 has one active destination (not a link stack).
            // Every start calls link(); end calls it only when that slot is occupied.
            match event {
                Event::Start(Tag::Link { dest_url, .. }) => {
                    let index = links.iter().position(|link| link.url == dest_url.as_ref());
                    depth += 1;
                    if depth <= super::MAX_LINKS {
                        stack.push(index);
                    }
                    let index = index.filter(|_| markers.len() + 2 <= 2 * super::MAX_LINKS);
                    if let Some(index) = index {
                        markers.push((expected, index));
                    }
                    active = Some(index);
                    expected += 1;
                }
                Event::End(TagEnd::Link) => {
                    if depth <= super::MAX_LINKS {
                        stack.pop();
                    }
                    depth = depth.saturating_sub(1);
                    if let Some(index) = active.take() {
                        if let Some(index) = index {
                            markers.push((expected, index));
                        }
                        expected += 1;
                    }
                }
                Event::Code(_) => {
                    if codes.len() < 2 * super::MAX_LINKS {
                        codes.push(if depth == 0 {
                            CodeMarker::Plain
                        } else if depth > super::MAX_LINKS {
                            CodeMarker::Unavailable
                        } else {
                            stack
                                .last()
                                .copied()
                                .flatten()
                                .map_or(CodeMarker::Unavailable, CodeMarker::Link)
                        });
                    }
                    expected_codes += 1;
                }
                _ => {}
            }
        }
        Self {
            calls: AtomicUsize::new(0),
            expected,
            markers,
            code_calls: AtomicUsize::new(0),
            expected_codes,
            codes,
        }
    }
    pub(super) fn next(&self) -> Color {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        self.markers
            .binary_search_by_key(&call, |(call, _)| *call)
            .ok()
            .map_or(Color::Reset, |at| {
                crate::terminal_links::marker_color(self.markers[at].1)
            })
    }
    pub(super) fn next_code(&self) -> Option<Color> {
        let call = self.code_calls.fetch_add(1, Ordering::Relaxed);
        match self.codes.get(call) {
            Some(CodeMarker::Plain) => None,
            Some(CodeMarker::Link(index)) => Some(crate::terminal_links::marker_color(*index)),
            Some(CodeMarker::Unavailable) | None => Some(Color::Reset),
        }
    }
    pub(super) fn complete(&self) -> bool {
        self.calls.load(Ordering::Relaxed) == self.expected
            && self.code_calls.load(Ordering::Relaxed) == self.expected_codes
    }
}
