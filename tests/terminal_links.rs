use ratatui::{
    backend::Backend,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use shellboy::terminal_links::*;
use std::{
    cell::RefCell,
    io::{self, Write},
    rc::Rc,
    sync::Arc,
};
use unicode_width::UnicodeWidthStr;
#[derive(Clone, Default)]
struct Bytes(Rc<RefCell<Vec<u8>>>);
impl Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn fixture(text: &str) -> (HyperlinkBackend<Bytes>, SharedLinkSnapshot, Bytes) {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 2));
    buffer.set_string(0, 0, text, Style::default().fg(Color::Cyan));
    let snapshot = Rc::new(RefCell::new(LinkSnapshot {
        buffer,
        regions: vec![],
    }));
    let bytes = Bytes::default();
    (
        HyperlinkBackend::new(bytes.clone(), snapshot.clone()),
        snapshot,
        bytes,
    )
}
fn region(x: u16, width: u16, url: &str) -> LinkRegion {
    LinkRegion {
        x,
        y: 0,
        width,
        url: Arc::from(url),
    }
}
fn recorded(bytes: &Bytes) -> String {
    String::from_utf8(bytes.0.borrow().clone()).unwrap()
}
fn empty_draw(backend: &mut HyperlinkBackend<Bytes>) {
    backend.draw(std::iter::empty()).unwrap();
}
#[test]
fn unchanged_glyph_url_changes_and_removal_force_redraw_without_cell_escape_hacks() {
    let (mut backend, snapshot, bytes) = fixture("label neighbour");
    let original = snapshot.borrow().buffer.clone();
    snapshot.borrow_mut().regions = vec![region(0, 5, "https://example.com/first?complete=123")];
    empty_draw(&mut backend);
    let first = recorded(&bytes);
    assert!(first.contains("\x1b]8;;https://example.com/first?complete=123\x1b\\"));
    assert!(first.contains("label"));
    assert!(first.ends_with("\x1b]8;;\x1b\\"));
    bytes.0.borrow_mut().clear();
    snapshot.borrow_mut().regions[0].url = Arc::from("https://example.com/second");
    empty_draw(&mut backend);
    let changed = recorded(&bytes);
    assert!(changed.contains("https://example.com/second"));
    assert!(changed.contains("label"));
    assert!(!changed.contains("neighbour"));
    bytes.0.borrow_mut().clear();
    snapshot.borrow_mut().regions.clear();
    empty_draw(&mut backend);
    let removed = recorded(&bytes);
    assert!(removed.contains("label"));
    assert!(!removed.contains("https://"));
    assert_eq!(snapshot.borrow().buffer, original);
    assert_eq!(snapshot.borrow().buffer[(0, 0)].symbol(), "l");
    assert!(
        snapshot
            .borrow()
            .buffer
            .content
            .iter()
            .all(|cell| !cell.symbol().contains('\x1b'))
    );
}
#[test]
fn region_move_redraws_old_and_new_same_text_and_closes_before_neighbours() {
    let (mut backend, snapshot, bytes) = fixture("aaBBcc");
    snapshot.borrow_mut().regions = vec![region(0, 2, "https://example.com")];
    let snapshot_ref = snapshot.borrow();
    backend
        .draw(
            snapshot_ref
                .buffer
                .content
                .iter()
                .enumerate()
                .take(6)
                .map(|(x, cell)| (x as u16, 0, cell)),
        )
        .unwrap();
    drop(snapshot_ref);
    let text = recorded(&bytes);
    let link = text.find("https://example.com").unwrap();
    let close = text[link..].find("\x1b]8;;\x1b\\").unwrap() + link;
    let bb = text.find("BB").unwrap();
    assert!(close < bb);
    bytes.0.borrow_mut().clear();
    snapshot.borrow_mut().regions = vec![region(2, 2, "https://example.com")];
    empty_draw(&mut backend);
    let moved = recorded(&bytes);
    assert!(moved.contains("aa"));
    assert!(moved.contains("BB"));
    assert!(!moved.contains("cc"));
}
#[test]
fn unicode_wide_continuation_never_overwrites_half_a_glyph() {
    let (mut backend, snapshot, bytes) = fixture("日🦀e\u{301}終");
    let buffer = snapshot.borrow().buffer.clone();
    assert_eq!(buffer[(0, 0)].symbol().width(), 2);
    assert_eq!(buffer[(2, 0)].symbol().width(), 2);
    // Deliberately mark only continuation cells; draw must normalize both to their lead.
    snapshot.borrow_mut().regions = vec![
        region(1, 1, "https://example.com/jp"),
        region(3, 1, "https://example.com/crab"),
    ];
    empty_draw(&mut backend);
    let output = recorded(&bytes);
    assert!(output.contains('日'));
    assert!(output.contains('🦀'));
    assert!(output.contains("\x1b[1;1H"));
    assert!(output.contains("\x1b[1;3H"));
    assert!(!output.contains("\x1b[1;2H"));
    assert!(!output.contains("\x1b[1;4H"));
    assert_eq!(snapshot.borrow().buffer, buffer);
    bytes.0.borrow_mut().clear();
    snapshot.borrow_mut().regions.clear();
    empty_draw(&mut backend);
    let output = recorded(&bytes);
    assert!(output.contains('日') && output.contains('🦀'));
    assert!(!output.contains("\x1b[1;2H"));
}
#[test]
fn invalid_targets_are_not_emitted_and_writer_requests_are_delegated() {
    let (mut backend, snapshot, bytes) = fixture("safe");
    for target in [
        "file:///tmp/private",
        "javascript:alert(1)",
        "https://example.com/\x1b]52;c;secret\x07",
        "https://example.com/\nspoof",
        "http://",
    ] {
        assert!(!valid_link_target(target));
        snapshot.borrow_mut().regions = vec![region(0, 4, target)];
        empty_draw(&mut backend);
        assert!(!recorded(&bytes).contains(target));
    }
    assert!(valid_link_target("https://example.com/日本語?full=1&x=2"));
    backend.write_all(b"EXPLICIT_TERMINAL_REQUEST").unwrap();
    assert!(recorded(&bytes).contains("EXPLICIT_TERMINAL_REQUEST"));
}
#[test]
fn repeated_stable_snapshot_emits_no_cells_and_overlay_removes_existing_links() {
    let (mut backend, snapshot, bytes) = fixture("anchor");
    snapshot.borrow_mut().regions = vec![region(0, 6, "https://example.com")];
    empty_draw(&mut backend);
    bytes.0.borrow_mut().clear();
    empty_draw(&mut backend);
    assert_eq!(recorded(&bytes), "\x1b]8;;\x1b\\");
    snapshot.borrow_mut().regions.clear();
    empty_draw(&mut backend);
    assert!(recorded(&bytes).contains("anchor"));
    // No URL recognition/access layer is invoked: this backend only validates and writes bytes.
    assert_eq!(snapshot.borrow().buffer[(0, 0)].symbol(), "a");
}

#[test]
fn terminal_clear_invalidates_link_metadata_and_redraws_current_viewport() {
    use ratatui::backend::ClearType;
    for full_screen in [false, true] {
        let (mut backend, snapshot, bytes) = fixture("old link unchanged");
        snapshot.borrow_mut().regions = vec![region(0, 3, "https://example.com")];
        empty_draw(&mut backend);
        if full_screen {
            backend.clear().unwrap();
        } else {
            backend.clear_region(ClearType::AfterCursor).unwrap();
        }
        snapshot.borrow_mut().regions.clear();
        bytes.0.borrow_mut().clear();
        empty_draw(&mut backend);
        let text = recorded(&bytes);
        assert!(text.contains("old link"));
        assert!(text.contains("unchanged"));
        assert!(!text.contains("https://"));
    }
}
