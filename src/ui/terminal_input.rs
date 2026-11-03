//! One input owner: terminal replies are separated before ordinary key decoding.
use super::capability_probe::{Capability, Kind, Reply, ReplyFilter};
use crossterm::event::{Event, KeyboardEnhancementFlags};
use std::{collections::VecDeque, io, time::Duration};

mod parse;

#[derive(Clone, Debug, PartialEq, Eq)]
enum InternalEvent {
    Event(Event),
    CursorPosition(u16, u16),
    KeyboardEnhancementFlags(KeyboardEnhancementFlags),
    PrimaryDeviceAttributes,
}

pub(super) struct Decoder {
    filter: ReplyFilter,
    bytes: Vec<u8>,
    events: VecDeque<Event>,
    pub width: Capability<u8>,
    pub keyboard: Capability<bool>,
    pub attributes: Capability<bool>,
    cursor: Vec<u16>,
}
impl Default for Decoder {
    fn default() -> Self {
        Self {
            filter: ReplyFilter::default(),
            bytes: Vec::new(),
            events: VecDeque::new(),
            width: Capability::Unmeasured,
            keyboard: Capability::Unmeasured,
            attributes: Capability::Unmeasured,
            cursor: Vec::with_capacity(2),
        }
    }
}
impl Decoder {
    pub fn queries(&mut self, measure_width: bool) -> &'static [u8] {
        self.filter.expect(Kind::Keyboard);
        self.filter.expect(Kind::DeviceAttributes);
        if measure_width {
            self.filter.expect(Kind::Cursor);
            self.filter.expect(Kind::Cursor);
            // Save/restore the cursor and erase the probe before the first frame.
            b"\x1b[?u\x1b[c\x1b7\x1b[11;1H\xc2\xb7\x1b[?6n\xc2\xb7\x1b[?6n\x1b[2K\x1b8"
        } else {
            b"\x1b[?u\x1b[c"
        }
    }
    fn reply(&mut self, reply: Reply) {
        match reply.kind {
            Kind::Keyboard => self.keyboard = Capability::Measured(true),
            Kind::DeviceAttributes => self.attributes = Capability::Measured(true),
            Kind::Cursor => {
                if reply.values[0] != 11 {
                    self.cursor.push(0);
                } else {
                    self.cursor.push(reply.values[1]);
                }
                if self.cursor.len() == 2 {
                    self.width = match self.cursor.as_slice() {
                        [2, 3] => Capability::Measured(1),
                        [3, 5] => Capability::Measured(2),
                        _ => Capability::Unmeasured,
                    };
                }
            }
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut input = Vec::with_capacity(bytes.len());
        let mut replies = Vec::new();
        self.filter.feed(bytes, &mut input, &mut replies);
        for reply in replies {
            self.reply(reply);
        }
        self.decode(&input);
    }
    fn decode(&mut self, input: &[u8]) {
        for byte in input {
            self.bytes.push(*byte);
            match parse::parse_event(&self.bytes, true) {
                Ok(Some(InternalEvent::Event(event))) => {
                    self.events.push_back(event);
                    self.bytes.clear();
                }
                Ok(Some(_)) | Err(_) => self.bytes.clear(),
                Ok(None) => {}
            }
        }
    }
    fn finish_escape(&mut self) {
        // Only a lone Escape is ambiguous with a key. Never time out a partial
        // capability reply and expose its tail as user input.
        let mut input = Vec::new();
        if self.filter.lone_escape() {
            self.filter.flush_input(&mut input);
            self.decode(&input);
        }
        if self.bytes == b"\x1b" {
            self.events
                .push_back(Event::Key(crossterm::event::KeyCode::Esc.into()));
            self.bytes.clear();
        }
    }
}

pub(super) struct Input {
    pub decoder: Decoder,
    #[cfg(unix)]
    tty: std::fs::File,
    size: (u16, u16),
}
impl Input {
    pub fn new() -> io::Result<Self> {
        #[cfg(unix)]
        let tty = {
            use std::os::{fd::FromRawFd, unix::fs::OpenOptionsExt};
            match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open("/dev/tty")
            {
                Ok(tty) => tty,
                Err(_) => {
                    // SAFETY: dup gives this session independent ownership of stdin's FD.
                    let fd = unsafe { libc::dup(libc::STDIN_FILENO) };
                    if fd < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    unsafe { std::fs::File::from_raw_fd(fd) }
                }
            }
        };
        Ok(Self {
            decoder: Decoder::default(),
            #[cfg(unix)]
            tty,
            size: crossterm::terminal::size()?,
        })
    }
    pub fn poll(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
        if let Some(event) = self.decoder.events.pop_front() {
            return Ok(Some(event));
        }
        let size = crossterm::terminal::size()?;
        if size != self.size {
            self.size = size;
            return Ok(Some(Event::Resize(size.0, size.1)));
        }
        #[cfg(windows)]
        {
            if crossterm::event::poll(timeout)? {
                return crossterm::event::read().map(Some);
            }
        }
        #[cfg(unix)]
        {
            use std::{io::Read, os::fd::AsRawFd};
            let mut fd = libc::pollfd {
                fd: self.tty.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live pollfd; the UI owns the only reader of this terminal.
            let result =
                unsafe { libc::poll(&mut fd, 1, timeout.as_millis().min(i32::MAX as u128) as i32) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                return Err(error);
            }
            if result > 0 {
                let mut bytes = [0; 4096];
                match self.tty.read(&mut bytes) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(n) => self.decoder.feed(&bytes[..n]),
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(e),
                }
            } else {
                self.decoder.finish_escape();
            }
        }
        Ok(self.decoder.events.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn typed(decoder: &mut Decoder) -> String {
        decoder
            .events
            .drain(..)
            .map(|event| match event {
                Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                }) => c,
                other => panic!("unexpected input: {other:?}"),
            })
            .collect()
    }
    #[test]
    fn silent_queries_leave_capabilities_unknown_and_typing_available() {
        let mut decoder = Decoder::default();
        assert!(!decoder.queries(true).is_empty());
        decoder.feed(b"hello");
        assert_eq!(typed(&mut decoder), "hello");
        assert_eq!(decoder.width, Capability::Unmeasured);
        assert_eq!(decoder.keyboard, Capability::Unmeasured);
        assert_eq!(decoder.attributes, Capability::Unmeasured);
    }
    #[test]
    fn keyboard_flags_never_wait_for_device_attributes() {
        let mut decoder = Decoder::default();
        decoder.queries(true);
        for fragment in b"a\x1b[?1ub\x1b[?11;2Rc\x1b[?11;3Rd".chunks(1) {
            decoder.feed(fragment);
        }
        assert_eq!(typed(&mut decoder), "abcd");
        assert_eq!(decoder.width, Capability::Measured(1));
        assert_eq!(decoder.keyboard, Capability::Measured(true));
        assert_eq!(decoder.attributes, Capability::Unmeasured);
        decoder.feed(b"later\x1b[?1;2c");
        assert_eq!(typed(&mut decoder), "later");
        assert_eq!(decoder.attributes, Capability::Measured(true));
    }
    #[test]
    fn paste_uses_existing_lossy_utf8_and_never_consumes_query_replies() {
        let mut decoder = Decoder::default();
        decoder.queries(true);
        for fragment in b"\x1b[200~\xff\x1b[?1u\x1b[?11;2R\x1b[201~".chunks(2) {
            decoder.feed(fragment);
        }
        assert_eq!(
            decoder.events.pop_front(),
            Some(Event::Paste("\u{fffd}\x1b[?1u\x1b[?11;2R".into()))
        );
        assert_eq!(decoder.keyboard, Capability::Unmeasured);
        assert_eq!(decoder.width, Capability::Unmeasured);
    }
    #[test]
    fn partial_reply_survives_idle_without_releasing_its_tail_as_keys() {
        let mut decoder = Decoder::default();
        decoder.queries(true);
        decoder.feed(b"\x1b[?11;");
        decoder.finish_escape();
        assert!(decoder.events.is_empty());
        decoder.feed(b"3R\x1b[?11;5Rz");
        assert_eq!(decoder.width, Capability::Measured(2));
        assert_eq!(typed(&mut decoder), "z");
    }
    #[test]
    fn escape_alt_arrows_and_queued_events_are_not_discarded() {
        let mut decoder = Decoder::default();
        decoder.feed(b"\x1b");
        decoder.finish_escape();
        assert_eq!(
            decoder.events.pop_front(),
            Some(Event::Key(KeyCode::Esc.into()))
        );
        decoder.feed(b"\x1bx\x1b[A");
        assert_eq!(
            decoder.events.pop_front(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::ALT
            )))
        );
        assert_eq!(
            decoder.events.pop_front(),
            Some(Event::Key(KeyCode::Up.into()))
        );
    }
    #[test]
    fn inconsistent_measurement_is_not_a_guess() {
        let mut decoder = Decoder::default();
        decoder.queries(true);
        decoder.feed(b"\x1b[?11;2R\x1b[?11;5R");
        assert_eq!(decoder.width, Capability::Unmeasured);
    }
}
