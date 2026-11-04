//! Synthetic cell/sequence evidence, not a Windows pixel or font test.
use super::*;
use ratatui::{
    backend::{ClearType, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};
use std::{cell::RefCell, io, rc::Rc};

#[derive(Clone, Default)]
struct Bytes(Rc<RefCell<Vec<u8>>>);
impl io::Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Output {
    bytes: Bytes,
    inner: crate::terminal_links::HyperlinkBackend<Bytes>,
    size: Size,
}
impl Output {
    fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.bytes.0.borrow_mut())
    }
}
impl Backend for Output {
    type Error = io::Error;
    fn draw<'a, I>(&mut self, cells: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(cells)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        panic!("redraw must not query physical cursor")
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> io::Result<()> {
        self.inner.set_cursor_position(p)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, kind: ClearType) -> io::Result<()> {
        self.inner.clear_region(kind)
    }
    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::new(0, 0),
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
fn terminal(width: u16) -> Terminal<Output> {
    let bytes = Bytes::default();
    Terminal::new(Output {
        inner: crate::terminal_links::HyperlinkBackend::new(bytes.clone(), links()),
        bytes,
        size: Size::new(width, 30),
    })
    .unwrap()
}
fn links() -> SharedLinkSnapshot {
    Rc::new(RefCell::new(LinkSnapshot::default()))
}
fn frame(terminal: &mut Terminal<Output>, ui: &mut UiState) -> Vec<u8> {
    let snapshot = terminal.backend().inner.snapshot();
    draw_terminal(terminal, ui, &snapshot).unwrap();
    terminal.backend_mut().take()
}
fn fixture() -> UiState {
    let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
    ui.select_thread("synthetic-history".into());
    let nodes = (0..40)
        .map(|i| UiNode {
            node_id: format!("node-{i:03}"),
            role: if i % 3 == 0 { "user" } else { "assistant" }.into(),
            text: format!(
                "Row {i:03}: synthetic conversation, arrows → ↑ ↓ · …\n\
            ◉ ⁂ $ ↳ ⋯ ├─ └─ 日本語の履歴\n\
            A realistic response with some **emphasis**, `code`, and a wrapped explanation.\n\
            Another line of synthetic content at item {i:03}."
            ),
            ..Default::default()
        })
        .collect();
    ui.apply(UiUpdate::History {
        pending: Default::default(),
        read_revision: 0,
        thread_id: "synthetic-history".into(),
        generation: ui.view().generation,
        total_nodes: 40,
        nodes,
        before_cursor: None,
        append: false,
        first_ordinal: 0,
        selection_revision: 0,
    });
    ui
}
fn differences(
    a: &vt100::Screen,
    b: &vt100::Screen,
    width: u16,
) -> Vec<(u16, u16, String, String)> {
    (0..30)
        .flat_map(|y| {
            (0..width).filter_map(move |x| {
                let (a, b) = (a.cell(y, x).unwrap(), b.cell(y, x).unwrap());
                let shown = |cell: &vt100::Cell| {
                    if cell.contents().is_empty() {
                        " ".to_owned()
                    } else {
                        cell.contents().to_owned()
                    }
                };
                (shown(a) != shown(b)
                    || a.fgcolor() != b.fgcolor()
                    || a.bgcolor() != b.bgcolor()
                    || a.bold() != b.bold()
                    || a.italic() != b.italic()
                    || a.underline() != b.underline()
                    || a.inverse() != b.inverse()
                    || a.is_wide() != b.is_wide()
                    || a.is_wide_continuation() != b.is_wide_continuation())
                .then(|| (x, y, shown(a), shown(b)))
            })
        })
        .collect()
}

#[test]
fn synthetic_help_width_mismatch_leaves_residue_in_diff() {
    text_width::set_terminal_width(Some(1));
    for enabled in [false, true] {
        let mut ui = fixture();
        ui.config.ui.clean_redraw = enabled;
        let mut terminal = terminal(80);
        let mut screen = vt100::Parser::new(30, 80, 0);
        screen.process(&frame(&mut terminal, &mut ui));
        ui.slash("/help");
        let help = frame(&mut terminal, &mut ui);
        assert!(String::from_utf8_lossy(&help).contains('→'));
        // vt100 has fixed Unicode widths. A two-cell CJK proxy models a terminal
        // advancing two cells for the app's one-cell arrow; it is not font evidence.
        let mismatch = String::from_utf8(help).unwrap().replace('→', "界");
        screen.process(mismatch.as_bytes());
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        let close = frame(&mut terminal, &mut ui);
        assert_eq!(close.windows(4).any(|b| b == b"\x1b[2J"), enabled);
        screen.process(&close);
        let mut fresh = vt100::Parser::new(30, 80, 0);
        ui.full_redraw_requested = true;
        fresh.process(&frame(&mut terminal, &mut ui));
        let diff = differences(screen.screen(), fresh.screen(), 80);
        eprintln!("help width mismatch enabled={enabled} final differing cells: {diff:?}");
        if enabled {
            assert!(diff.is_empty(), "full redraw must match fresh: {diff:?}");
        } else {
            assert!(
                !diff.is_empty(),
                "baseline should expose the synthetic residue"
            );
        }
    }
    text_width::clear_terminal_width();
}

#[test]
fn overlay_close_clears_once_including_help_above_a_retained_popup() {
    for enabled in [false, true] {
        let mut ui = fixture();
        ui.config.ui.clean_redraw = enabled;
        let mut terminal = terminal(100);
        frame(&mut terminal, &mut ui);
        ui.event(Event::Key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE)));
        assert!(ui.operations.is_some());
        frame(&mut terminal, &mut ui);
        ui.event(Event::Key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)));
        assert!(ui.help_visible);
        assert!(ui.operations.is_some());
        frame(&mut terminal, &mut ui);
        ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!ui.help_visible);
        assert!(ui.operations.is_some());
        let close = frame(&mut terminal, &mut ui);
        assert_eq!(close.windows(4).any(|b| b == b"\x1b[2J"), enabled);
        assert!(
            !frame(&mut terminal, &mut ui)
                .windows(4)
                .any(|b| b == b"\x1b[2J")
        );
        ui.event(Event::Key(KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
        )));
        assert!(
            frame(&mut terminal, &mut ui)
                .windows(4)
                .any(|b| b == b"\x1b[2J")
        );
    }
}

#[test]
fn moved_history_rows_erase_and_rewrite_both_edges_with_both_settings() {
    text_width::set_terminal_width(Some(1));
    for width in [80, 100, 120] {
        for page in [false, true] {
            for enabled in [false, true] {
                let mut ui = fixture();
                ui.config.ui.clean_redraw = enabled;
                let mut terminal = terminal(width);
                let mut screen = vt100::Parser::new(30, width, 0);
                screen.process(&frame(&mut terminal, &mut ui));
                let area = ui.view().navigation.scroll.area.unwrap();
                let y = area.y + 1;
                // Explicit pixel-overhang surrogate: cell contamination at both
                // physical edges, not an assertion about Windows font rasterization.
                screen.process(format!("\x1b[{};1H#\x1b[{};{}H@", y + 1, y + 1, width).as_bytes());
                let row = ui.view().navigation.scroll.row();
                if page {
                    ui.event(Event::Key(KeyEvent::new(
                        KeyCode::PageUp,
                        KeyModifiers::NONE,
                    )));
                } else {
                    ui.scroll(-1);
                }
                let bytes = frame(&mut terminal, &mut ui);
                assert_ne!(
                    ui.view().navigation.scroll.row(),
                    row,
                    "fixture must actually move"
                );
                let erases = bytes.windows(3).filter(|b| *b == b"\x1b[K").count();
                assert_eq!(
                    erases,
                    if enabled {
                        usize::from(area.height - 2)
                    } else {
                        0
                    }
                );
                screen.process(&bytes);
                let mut fresh = vt100::Parser::new(30, width, 0);
                ui.full_redraw_requested = true;
                fresh.process(&frame(&mut terminal, &mut ui));
                let diff = differences(screen.screen(), fresh.screen(), width);
                eprintln!(
                    "edge surrogate width={width} page={page} enabled={enabled} cells={diff:?}"
                );
                if enabled {
                    assert!(diff.is_empty(), "{diff:?}");
                } else {
                    assert!(diff.iter().any(|c| c.0 == 0 && c.1 == y));
                    assert!(diff.iter().any(|c| c.0 == width - 1 && c.1 == y));
                }
                // No idle repair, no repair behind help, no changes to the draft.
                assert!(
                    !frame(&mut terminal, &mut ui)
                        .windows(3)
                        .any(|b| b == b"\x1b[K")
                );
                ui.slash("/help");
                assert!(
                    !frame(&mut terminal, &mut ui)
                        .windows(3)
                        .any(|b| b == b"\x1b[K")
                );
                assert!(ui.view().draft.text.is_empty());
            }
        }
    }
    text_width::clear_terminal_width();
}

#[test]
fn synthetic_scroll_width_mismatch_reports_every_differing_cell() {
    text_width::set_terminal_width(Some(1));
    for page in [false, true] {
        let mut ui = fixture();
        ui.config.ui.clean_redraw = false;
        let mut terminal = terminal(80);
        let mut screen = vt100::Parser::new(30, 80, 0);
        let widen = |bytes: Vec<u8>| {
            String::from_utf8(bytes)
                .unwrap()
                .replace('→', "界")
                .into_bytes()
        };
        screen.process(&widen(frame(&mut terminal, &mut ui)));
        if page {
            ui.event(Event::Key(KeyEvent::new(
                KeyCode::PageUp,
                KeyModifiers::NONE,
            )));
        } else {
            ui.scroll(-1);
        }
        screen.process(&widen(frame(&mut terminal, &mut ui)));
        let mut fresh = vt100::Parser::new(30, 80, 0);
        ui.full_redraw_requested = true;
        fresh.process(&widen(frame(&mut terminal, &mut ui)));
        let diff = differences(screen.screen(), fresh.screen(), 80);
        eprintln!("scroll two-cell-arrow page={page} differing cells={diff:?}");
        assert!(
            !diff.is_empty(),
            "model should expose baseline width residue"
        );
    }
    text_width::clear_terminal_width();
}

#[test]
fn measure_clean_redraw_bytes_on_realistic_history() {
    eprintln!("CLEAN_REDRAW_BYTES policy,width,event,off,on,added");
    for (policy, observation) in [("unknown", None), ("narrow", Some(1)), ("wide", Some(2))] {
        text_width::set_terminal_width(observation);
        for width in [80, 100, 120] {
            for event in ["line_up", "page_up", "overlay_close"] {
                let mut counts = Vec::new();
                for enabled in [false, true] {
                    let mut ui = fixture();
                    ui.config.ui.clean_redraw = enabled;
                    let mut terminal = terminal(width);
                    frame(&mut terminal, &mut ui);
                    match event {
                        "line_up" => {
                            ui.scroll(-1);
                        }
                        "page_up" => {
                            ui.event(Event::Key(KeyEvent::new(
                                KeyCode::PageUp,
                                KeyModifiers::NONE,
                            )));
                        }
                        _ => {
                            ui.slash("/help");
                            frame(&mut terminal, &mut ui);
                            ui.event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
                        }
                    }
                    counts.push(frame(&mut terminal, &mut ui).len());
                }
                eprintln!(
                    "CLEAN_REDRAW_BYTES {policy},{width},{event},{},{},{}",
                    counts[0],
                    counts[1],
                    counts[1] as isize - counts[0] as isize
                );
            }
        }
    }
    text_width::clear_terminal_width();
}

#[test]
fn measure_clean_redraw_on_sparse_log_history() {
    text_width::set_terminal_width(None);
    eprintln!("CLEAN_REDRAW_LOG_BYTES width,event,off,on,added");
    for width in [80, 100, 120] {
        for page in [false, true] {
            let mut counts = Vec::new();
            for enabled in [false, true] {
                let mut ui = fixture();
                ui.config.ui.clean_redraw = enabled;
                let text = format!(
                    "```text\n{}```",
                    (0..200)
                        .map(|i| format!(
                            "2026-11-04 12:{:02}:00 INFO task={i:03} status=complete 日本語 →\n",
                            i % 60
                        ))
                        .collect::<String>()
                );
                ui.apply(UiUpdate::History {
                    pending: Default::default(),
                    read_revision: 0,
                    thread_id: "synthetic-history".into(),
                    generation: ui.view().generation,
                    total_nodes: 1,
                    nodes: vec![UiNode {
                        node_id: "synthetic-log".into(),
                        role: "assistant".into(),
                        total_chars: text.chars().count(),
                        text,
                        ..Default::default()
                    }],
                    before_cursor: None,
                    append: false,
                    first_ordinal: 0,
                    selection_revision: 0,
                });
                let mut terminal = terminal(width);
                frame(&mut terminal, &mut ui);
                let row = ui.view().navigation.scroll.row();
                if page {
                    ui.event(Event::Key(KeyEvent::new(
                        KeyCode::PageUp,
                        KeyModifiers::NONE,
                    )));
                } else {
                    ui.scroll(-1);
                }
                let bytes = frame(&mut terminal, &mut ui);
                assert_ne!(row, ui.view().navigation.scroll.row());
                counts.push(bytes.len());
            }
            eprintln!(
                "CLEAN_REDRAW_LOG_BYTES {width},{},{},{},{}",
                if page { "page_up" } else { "line_up" },
                counts[0],
                counts[1],
                counts[1] as isize - counts[0] as isize
            );
        }
    }
    text_width::clear_terminal_width();
}
