//! Terminal mode restoration owned by the single terminal session.
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture},
    execute,
    terminal::{self, LeaveAlternateScreen},
};
use std::io;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::ThreadId;

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

struct Restore {
    owner: ThreadId,
    active: AtomicBool,
}
impl Restore {
    fn restore_owner(&self) {
        if std::thread::current().id() != self.owner || !self.active.swap(false, Ordering::AcqRel) {
            return;
        }
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}
pub(super) struct TerminalGuard {
    restore: Arc<Restore>,
    previous: Option<Arc<PanicHook>>,
}
impl TerminalGuard {
    pub(super) fn new() -> Self {
        let previous = Arc::new(std::panic::take_hook());
        let restore = Arc::new(Restore {
            owner: std::thread::current().id(),
            active: AtomicBool::new(true),
        });
        let hook_previous = Arc::clone(&previous);
        let hook_restore = Arc::clone(&restore);
        std::panic::set_hook(Box::new(move |info| {
            hook_restore.restore_owner();
            hook_previous(info);
        }));
        Self {
            restore,
            previous: Some(previous),
        }
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore.restore_owner();
        // Rust forbids hook replacement from a panicking thread. In that fatal
        // path the installed hook remains an inactive delegate until process exit.
        if !std::thread::panicking() {
            drop(std::panic::take_hook());
            let previous = self.previous.take().expect("session owns previous hook");
            std::panic::set_hook(match Arc::try_unwrap(previous) {
                Ok(previous) => previous,
                Err(previous) => Box::new(move |info| previous(info)),
            });
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs::File,
        io::Read,
        os::fd::{AsRawFd, FromRawFd},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    const FIXTURE: &str = "SHELLBOY_TERMINAL_PANIC_FIXTURE";
    const RESTORE: &[u8] = b"\x1b[?1049l";
    const DIAGNOSTIC: &[u8] = b"PRIOR_HOOK_DIAGNOSTIC";

    #[test]
    fn terminal_panic_fixture() {
        let Ok(mode) = std::env::var(FIXTURE) else {
            return;
        };
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            eprintln!("PRIOR_HOOK_DIAGNOSTIC");
            previous(info);
        }));
        let session = crate::ui::TerminalSession::enter().unwrap();
        if mode == "owner" {
            panic!("FATAL_UI_OWNER_PANIC");
        }
        assert!(
            std::thread::spawn(|| panic!("RECOVERABLE_WORKER_PANIC"))
                .join()
                .is_err()
        );
        eprintln!("UI_STILL_ACTIVE");
        drop(session);
        eprintln!("SESSION_CLOSED");
        // Normal session Drop must restore the previous hook as well as modes.
        assert!(std::panic::catch_unwind(|| panic!("AFTER_SESSION_PANIC")).is_err());
    }
    fn run_fixture(mode: &str) -> (std::process::ExitStatus, Vec<u8>) {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: initialized output pointers; each successful FD becomes one owned File.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::addr_of_mut!(size),
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        // SAFETY: only our live master descriptor is inspected and updated.
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ui::terminal_guard::tests::terminal_panic_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(FIXTURE, mode)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .unwrap();
        drop(slave);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut output = Vec::new();
        loop {
            let mut bytes = [0; 8192];
            match master.read(&mut bytes) {
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.raw_os_error() == Some(libc::EIO) => {}
                Err(e) => panic!("fixture PTY: {e}"),
            }
            assert!(output.len() < 64 * 1024, "fixture output bounded");
            if let Some(status) = child.try_wait().unwrap() {
                // Drain while the child runs above; final bytes can still remain queued.
                while let Ok(n) = master.read(&mut bytes) {
                    if n == 0 {
                        break;
                    }
                    output.extend_from_slice(&bytes[..n]);
                }
                return (status, output);
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("fixture exceeded20s: {}", String::from_utf8_lossy(&output));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn at(bytes: &[u8], needle: &[u8]) -> usize {
        bytes
            .windows(needle.len())
            .position(|v| v == needle)
            .unwrap_or_else(|| {
                panic!(
                    "missing {:?}: {}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(bytes)
                )
            })
    }
    #[test]
    fn owner_panic_restores_real_terminal_before_delegating_the_diagnostic() {
        let (status, output) = run_fixture("owner");
        assert!(!status.success(), "fixture panic must remain a failure");
        assert!(at(&output, RESTORE) < at(&output, DIAGNOSTIC));
        assert!(at(&output, DIAGNOSTIC) < at(&output, b"FATAL_UI_OWNER_PANIC"));
        assert_eq!(
            output
                .windows(RESTORE.len())
                .filter(|v| *v == RESTORE)
                .count(),
            1
        );
    }
    #[test]
    fn worker_panic_keeps_terminal_active_and_normal_drop_restores_once() {
        let (status, output) = run_fixture("worker");
        assert!(status.success(), "{}", String::from_utf8_lossy(&output));
        assert!(at(&output, DIAGNOSTIC) < at(&output, b"UI_STILL_ACTIVE"));
        assert!(at(&output, b"UI_STILL_ACTIVE") < at(&output, RESTORE));
        assert!(at(&output, RESTORE) < at(&output, b"SESSION_CLOSED"));
        assert_eq!(
            output
                .windows(RESTORE.len())
                .filter(|v| *v == RESTORE)
                .count(),
            1
        );
        assert_eq!(
            output
                .windows(DIAGNOSTIC.len())
                .filter(|v| *v == DIAGNOSTIC)
                .count(),
            2
        );
    }
}
