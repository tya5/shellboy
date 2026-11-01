//! Shellboy layout policy, independent of Ratatui's upstream width tables.
use std::sync::atomic::{AtomicU8, Ordering};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr as UpstreamWidth};

static OBSERVED: AtomicU8 = AtomicU8::new(0);
#[cfg(test)]
thread_local! { static OVERRIDE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }

pub(crate) fn set_process_terminal_width(value: Option<u8>) {
    OBSERVED.store(
        value.filter(|v| *v == 1 || *v == 2).unwrap_or(3),
        Ordering::Release,
    );
}
pub(crate) fn clear_process_terminal_width() {
    OBSERVED.store(0, Ordering::Release);
}
fn policy() -> u8 {
    #[cfg(test)]
    let value = OVERRIDE.with(|v| v.get());
    #[cfg(not(test))]
    let value = 0;
    if value == 0 {
        OBSERVED.load(Ordering::Acquire)
    } else {
        value
    }
}
pub(crate) fn terminal_width_observation() -> Option<u8> {
    match policy() {
        1 => Some(1),
        2 => Some(2),
        _ => None,
    }
}
#[cfg(test)]
pub(crate) fn set_terminal_width(value: Option<u8>) {
    OVERRIDE.with(|v| v.set(value.filter(|v| *v == 1 || *v == 2).unwrap_or(3)));
}
#[cfg(test)]
pub(crate) fn clear_terminal_width() {
    OVERRIDE.with(|v| v.set(0));
}

pub(crate) trait UnicodeWidthStr {
    fn width(&self) -> usize;
    #[cfg(test)]
    fn width_cjk(&self) -> usize;
}
impl UnicodeWidthStr for str {
    fn width(&self) -> usize {
        let policy = policy();
        if policy == 0 {
            return UpstreamWidth::width(self);
        }
        let wide = policy >= 2;
        let ordinary = if wide {
            UpstreamWidth::width_cjk(self)
        } else {
            UpstreamWidth::width(self)
        };
        if self.contains(['\u{200d}', '\u{fe0f}', '\u{20e3}'])
            || self
                .chars()
                .any(|c| ('\u{1f3fb}'..='\u{1f3ff}').contains(&c))
        {
            let mut total: usize = 0;
            let mut previous = 0;
            for c in self.chars() {
                if matches!(c, '\u{fe0f}' | '\u{20e3}') && previous == 1 {
                    total += 1;
                }
                previous = if wide { c.width_cjk() } else { c.width() }.unwrap_or(0);
                if ('\u{1f3fb}'..='\u{1f3ff}').contains(&c) {
                    previous = previous.max(2);
                }
                total = total.saturating_add(previous);
            }
            ordinary.max(total)
        } else {
            ordinary
        }
    }
    #[cfg(test)]
    fn width_cjk(&self) -> usize {
        UpstreamWidth::width_cjk(self)
    }
}

#[cfg(test)]
pub(crate) fn is_emoji_variation_base(c: char) -> bool {
    use unicode_properties::UnicodeEmoji;
    !c.is_ascii() && c.is_emoji_char()
}

/// Called in an isolated helper process so a timed-out DSR cannot retain the
/// crossterm event-reader lock or consume input once the UI starts.
pub fn probe_child() -> i32 {
    use std::io::Write;
    // Crossterm tracks raw mode per process. Mark it enabled in the helper so
    // position() does not disable the parent's already-active terminal mode.
    if crossterm::terminal::enable_raw_mode().is_err() {
        return 3;
    }
    let mut out = std::io::stdout();
    if out
        .write_all("\x1b[11;1H·".as_bytes())
        .and_then(|_| out.flush())
        .is_err()
    {
        return 3;
    }
    let first = crossterm::cursor::position();
    if out
        .write_all("·".as_bytes())
        .and_then(|_| out.flush())
        .is_err()
    {
        return 3;
    }
    let second = crossterm::cursor::position();
    match (first, second) {
        (Ok((1, 10)), Ok((2, 10))) => 1,
        (Ok((2, 10)), Ok((4, 10))) => 2,
        _ => 3,
    }
}

pub(super) fn probe() -> Option<u8> {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    if std::env::var_os("SHELLBOY_INTERNAL_WIDTH_PROBE").is_some() {
        return None;
    }
    if !crossterm::terminal::size().is_ok_and(|(cols, rows)| cols >= 5 && rows >= 11) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let mut command = Command::new(exe);
    command
        .env("SHELLBOY_INTERNAL_WIDTH_PROBE", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null());
    #[cfg(test)]
    command.args([
        "--exact",
        "ui::text_width_pty_tests::probe_child_fixture",
        "--nocapture",
    ]);
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_millis(300);
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break match status.code() {
                    Some(1) => Some(1),
                    Some(2) => Some(2),
                    _ => None,
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    use std::io::Write;
    let _ = std::io::stdout().write_all(b"\x1b[2J\x1b[H");
    let _ = std::io::stdout().flush();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glyph_separator_requires_a_measured_single_column() {
        for (observed, expected) in [(Some(1), " · "), (Some(2), " | "), (None, " | ")] {
            set_terminal_width(observed);
            assert_eq!(glyphs::separator(), expected);
            assert_eq!(UnicodeWidthStr::width(glyphs::separator()), 3);
        }
        clear_terminal_width();
    }

    #[test]
    fn measured_and_unknown_width_are_conservative() {
        set_terminal_width(Some(1));
        assert_eq!(UnicodeWidthStr::width("A·日"), 4);
        set_terminal_width(Some(2));
        assert_eq!(UnicodeWidthStr::width("A·日"), 5);
        set_terminal_width(None);
        assert_eq!(UnicodeWidthStr::width("A·日"), 5);
        assert_eq!(UnicodeWidthStr::width("👩\u{200d}💻"), 4);
        assert_eq!(UnicodeWidthStr::width("❤\u{fe0f}"), 2);
        clear_terminal_width();
    }
}

/// App-owned separators selected only from the measured terminal policy.
pub(crate) mod glyphs {
    pub(crate) fn separator() -> &'static str {
        if super::terminal_width_observation() == Some(1) {
            " · "
        } else {
            " | "
        }
    }
}
