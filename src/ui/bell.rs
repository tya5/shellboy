//! One pending terminal notification; no timer, task, queue, or per-Thread cache.
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

const COALESCE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct Bell {
    pending: bool,
    last_sent: Option<Instant>,
}

impl Bell {
    pub(super) fn request(&mut self, enabled: bool, now: Instant) {
        if enabled
            && self
                .last_sent
                .is_none_or(|last| now.duration_since(last) >= COALESCE)
        {
            self.pending = true;
        }
    }

    /// Called only by the terminal loop, between terminal drawing operations.
    /// Notifications within the interval are coalesced, never delayed by a timer.
    pub(super) fn flush(&mut self, writer: &mut impl Write, now: Instant) -> io::Result<()> {
        if self.pending {
            writer.write_all(b"\x07")?;
            writer.flush()?;
            self.pending = false;
            self.last_sent = Some(now);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        ui::{UiState, UiUpdate},
    };

    #[test]
    fn terminal_backend_receives_exactly_one_bel_and_no_cursor_commands() {
        use crate::terminal_links::{HyperlinkBackend, LinkSnapshot};
        use std::{cell::RefCell, rc::Rc};
        #[derive(Clone)]
        struct Writer(Rc<RefCell<Vec<u8>>>);
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let mut backend = HyperlinkBackend::new(
            Writer(Rc::clone(&bytes)),
            Rc::new(RefCell::new(LinkSnapshot::default())),
        );
        let mut bell = Bell::default();
        let now = Instant::now();
        bell.flush(&mut backend, now).unwrap();
        assert!(bytes.borrow().is_empty(), "startup must remain silent");
        bell.request(true, now);
        bell.request(true, now);
        bell.flush(&mut backend, now).unwrap();
        bell.flush(&mut backend, now).unwrap();
        assert_eq!(&*bytes.borrow(), b"\x07");
        bell.request(true, now + Duration::from_millis(1999));
        bell.flush(&mut backend, now + Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            &*bytes.borrow(),
            b"\x07",
            "coalesced notifications do not schedule a later bell"
        );
        bell.request(true, now + Duration::from_secs(2));
        bell.flush(&mut backend, now + Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            &*bytes.borrow(),
            b"\x07\x07",
            "a later recovery may notify again"
        );
    }

    #[test]
    fn disabled_bell_is_silent_and_global_notification_keeps_ui_state() {
        for enabled in [false, true] {
            let mut config = Config::default();
            config.ui.bell = enabled;
            let mut ui = UiState::new(&config, std::env::temp_dir());
            ui.insert("keep draft", false);
            ui.selection_mode = true;
            ui.help_visible = true;
            ui.dirty = false;
            let active = ui.active.clone();
            ui.apply(UiUpdate::RecoveryBell);
            let mut bytes = Vec::new();
            ui.bell.flush(&mut bytes, Instant::now()).unwrap();
            assert_eq!(bytes, if enabled { vec![7] } else { vec![] });
            assert_eq!(ui.active, active);
            assert_eq!(ui.view().draft.text, "keep draft");
            assert!(ui.selection_mode && ui.help_visible);
            assert!(!ui.dirty, "notification must not require a redraw");
        }
    }
}
