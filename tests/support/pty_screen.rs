//! A persistent VT screen, not a search through independently emitted diff bytes.
use std::{fs::File, io::Read};
use tokio::io::unix::AsyncFd;

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

pub struct Screen {
    parser: vt100::Parser,
    boundary: Vec<u8>,
    pub raw: Vec<u8>,
    pub completed: String,
    pub frames: usize,
}

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
            boundary: Vec::new(),
            raw: Vec::new(),
            completed: String::new(),
            frames: 0,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        assert!(
            self.raw.len() + bytes.len() <= OUTPUT_LIMIT,
            "PTY output flood\n{}",
            self.diagnostic()
        );
        self.raw.extend_from_slice(bytes);
        for byte in bytes {
            self.parser.process(std::slice::from_ref(byte));
            self.boundary.push(*byte);
            if self.boundary.len() > 8 {
                self.boundary.remove(0);
            }
            // Ratatui ends each draw by hiding/showing the cursor. Keep cells
            // across draws, but never accept a marker from a half-written frame.
            if self.boundary.ends_with(b"\x1b[?25l") || self.boundary.ends_with(b"\x1b[?25h") {
                self.completed = self.parser.screen().contents();
                self.frames += 1;
            }
        }
    }

    pub fn diagnostic(&self) -> String {
        format!(
            "last completed screen ({} frames):\n{}\nlast live screen:\n{}\nSHUTDOWN_VT_CAPTURE_JSON={}",
            self.frames,
            self.completed,
            self.parser.screen().contents(),
            serde_json::to_string(&String::from_utf8_lossy(&self.raw)).unwrap(),
        )
    }
}

pub struct Pty {
    terminal: AsyncFd<File>,
    pub screen: Screen,
    after_frame: usize,
    after_byte: usize,
    reported_diff: bool,
}

impl Pty {
    pub fn new(terminal: File, rows: u16, cols: u16) -> Self {
        Self {
            // SAFETY: File owns this descriptor; it is never replaced or closed
            // while registered and remains private to this disposable fixture.
            terminal: unsafe { AsyncFd::register(terminal) }.unwrap(),
            screen: Screen::new(rows, cols),
            after_frame: 0,
            after_byte: 0,
            reported_diff: false,
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        use std::io::Write;
        self.after_frame = self.screen.frames;
        self.after_byte = self.screen.raw.len();
        self.terminal.get_ref().write_all(bytes).unwrap();
    }

    async fn read(&mut self) {
        loop {
            let mut ready = self.terminal.readable().await.unwrap();
            let mut bytes = [0; 8192];
            match ready.try_io(|fd| fd.get_ref().read(&mut bytes)) {
                Ok(Ok(n)) if n > 0 => {
                    self.screen.feed(&bytes[..n]);
                    return;
                }
                Err(_) => continue,
                result => panic!(
                    "PTY ended/read failed: {result:?}\n{}",
                    self.screen.diagnostic()
                ),
            }
        }
    }

    pub async fn until_screen(&mut self, labels: &[&str]) {
        let result = crate::test_watchdog::watchdog("PTY completed screen", async {
            loop {
                if self.screen.frames > self.after_frame
                    && labels
                        .iter()
                        .all(|label| self.screen.completed.contains(label))
                {
                    if !self.reported_diff && labels.contains(&"W/Enter:")
                        && !self.screen.raw[self.after_byte..].windows(8).any(|part| part == b"W/Enter:")
                    {
                        self.reported_diff = true;
                        eprintln!("PTY_DIFF_STATE: W/Enter: exists on screen but not in action output\n{}", self.screen.diagnostic());
                    }
                    return;
                }
                self.read().await;
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "Expected terminal screen {labels:?} did not arrive: {result:?}\n{}",
            self.screen.diagnostic()
        );
    }

    pub async fn until_raw(&mut self, marker: &[u8]) {
        let result = crate::test_watchdog::watchdog("PTY final terminal restoration", async {
            while !self.screen.raw[self.after_byte..]
                .windows(marker.len())
                .any(|part| part == marker)
            {
                self.read().await;
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "Expected terminal output {marker:?} did not arrive: {result:?}\n{}",
            self.screen.diagnostic()
        );
    }

    pub fn drain(&mut self) {
        loop {
            let mut bytes = [0; 8192];
            match self.terminal.get_ref().read(&mut bytes) {
                Ok(0) => return,
                Ok(n) => self.screen.feed(&bytes[..n]),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    return;
                }
                Err(error) => panic!("PTY drain failed: {error}\n{}", self.screen.diagnostic()),
            }
        }
    }
}

#[test]
fn reused_cell_and_fragmented_frames_are_screen_state_not_raw_text() {
    let mut screen = Screen::new(28, 100);
    screen.feed(b"\x1b[14;12H:\x1b[?25l");
    let first_frame = screen.frames;
    // Observed failure shape: the colon is unchanged, so Ratatui emits W/Enter
    // then jumps over the retained colon/blank to the remaining text.
    let diff = b"\x1b[14;2H | W/Enter\x1b[0m\x1b]8;;\x1b\\\x1b[14;14HKeep waiting  F: Choose force quit\x1b[?25l";
    for byte in &diff[..diff.len() - 1] {
        screen.feed(std::slice::from_ref(byte));
    }
    assert_eq!(screen.frames, first_frame);
    assert!(!screen.completed.contains("W/Enter:"));
    screen.feed(&diff[diff.len() - 1..]);
    assert!(screen.completed.contains("W/Enter:"));
    assert!(!screen.raw.windows(8).any(|part| part == b"W/Enter:"));
    assert!(screen.diagnostic().contains("W/Enter:"));
}

#[test]
fn overwritten_markers_do_not_satisfy_a_new_screen_state() {
    let mut screen = Screen::new(28, 100);
    screen.feed(b"\x1b[2J\x1b[HW/Enter:\x1b[?25l");
    assert!(screen.completed.contains("W/Enter:"));
    screen.feed(b"\x1b[2J\x1b[HN / Esc Back | Y Force quit\x1b[?25l");
    assert!(!screen.completed.contains("W/Enter:"));
    assert!(screen.completed.contains("N / Esc Back | Y Force quit"));
    screen.feed(b"\x1b[2J\x1b[HW/Enter:\x1b[?25l");
    assert!(screen.completed.contains("W/Enter:"));
    assert!(!screen.completed.contains("Y Force quit"));
}

#[test]
fn real_ratatui_diff_retains_the_colon_without_emitting_the_marker() {
    use ratatui::{
        Terminal, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect,
        widgets::Paragraph,
    };
    let mut output = Vec::new();
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(&mut output),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 100, 28)),
        },
    )
    .unwrap();
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new("xxxxxxx: waiting"), frame.area()))
        .unwrap();
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new("W/Enter: Keep waiting"), frame.area()))
        .unwrap();
    drop(terminal);
    let mut screen = Screen::new(28, 100);
    screen.feed(&output);
    assert!(screen.completed.contains("W/Enter: Keep waiting"));
    assert!(!output.windows(8).any(|part| part == b"W/Enter:"));
}

#[tokio::test(start_paused = true)]
async fn watchdog_failure_prints_the_last_screen_state() {
    let (terminal, _slave) = super::pty();
    let mut terminal = Pty::new(terminal, 28, 100);
    terminal
        .screen
        .feed(b"\x1b[Hretained shutdown screen\x1b[?25l");
    let failure = tokio::spawn(async move { terminal.until_screen(&["missing marker"]).await })
        .await
        .unwrap_err()
        .into_panic();
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.contains("last completed screen"));
    assert!(message.contains("retained shutdown screen"));
    assert!(message.contains("SHUTDOWN_VT_CAPTURE_JSON="));
}
