//! Synthetic cell/sequence evidence, not a Windows pixel or font test.
use super::*;
use ratatui::{
    backend::{ClearType, CrosstermBackend, WindowSize},
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
    inner: CrosstermBackend<Bytes>,
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
        inner: CrosstermBackend::new(bytes.clone()),
        bytes,
        size: Size::new(width, 30),
    })
    .unwrap()
}
fn links() -> SharedLinkSnapshot {
    Rc::new(RefCell::new(LinkSnapshot::default()))
}
fn frame(terminal: &mut Terminal<Output>, ui: &mut UiState) -> Vec<u8> {
    draw_terminal(terminal, ui, &links()).unwrap();
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
