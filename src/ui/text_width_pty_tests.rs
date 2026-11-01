//! Real PTY tests for the query, including input arriving between DSR replies.
use super::*;
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Command, Stdio},
    time::Duration,
};

const FIXTURE: &str = "SHELLBOY_WIDTH_PROBE_FIXTURE";

#[test]
fn probe_child_fixture() {
    if std::env::var_os("SHELLBOY_INTERNAL_WIDTH_PROBE").is_some() {
        std::process::exit(super::text_width::probe_child());
    }
}

#[test]
fn probe_fixture() {
    if std::env::var_os(FIXTURE).is_none() {
        return;
    }
    let session = TerminalSession::enter().unwrap();
    let result = session.width_observation;
    let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(libc::STDIN_FILENO, mode.as_mut_ptr()) },
        0
    );
    let mode = unsafe { mode.assume_init() };
    assert_eq!(
        mode.c_lflag & (libc::ICANON | libc::ECHO),
        0,
        "probe must leave parent terminal in raw mode"
    );
    drop(session);
    println!("PROBE_RESULT={result:?}");
}

fn run_fixture(replies: Option<(&str, &str)>) -> String {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: output pointers and size are initialized; each FD becomes one File.
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
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "ui::text_width_pty_tests::probe_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FIXTURE, "1")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    // SAFETY: only async-signal-safe syscalls run before exec; fd 0 is the
    // disposable slave, not the real user's controlling terminal.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(slave);
    let deadline = crate::test_watchdog::Deadline::new("width probe PTY");
    let mut output = Vec::new();
    let mut sent = 0;
    loop {
        let mut bytes = [0; 8192];
        match master.read(&mut bytes) {
            Ok(n) => output.extend_from_slice(&bytes[..n]),
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(libc::EIO) => {
            }
            Err(e) => panic!("probe PTY read: {e}"),
        }
        assert!(output.len() < 64 * 1024);
        let queries = output.windows(4).filter(|b| *b == b"\x1b[6n").count();
        if let Some((first, second)) = replies {
            if sent == 0 && queries >= 1 {
                master.write_all(first.as_bytes()).unwrap();
                sent = 1;
            }
            if sent == 1 && queries >= 2 {
                master.write_all(second.as_bytes()).unwrap();
                sent = 2;
            }
        }
        if let Some(status) = child.try_wait().unwrap() {
            while let Ok(n) = master.read(&mut bytes) {
                if n == 0 {
                    break;
                }
                output.extend_from_slice(&bytes[..n]);
            }
            let text = String::from_utf8_lossy(&output).into_owned();
            assert!(status.success(), "{text}");
            return text;
        }
        if deadline.expired() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "probe fixture watchdog: {}",
                String::from_utf8_lossy(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn probe_handles_one_two_inconsistent_and_timeout() {
    for (reply, expected) in [
        (Some(("\x1b[11;2R", "\x1b[11;3R")), "Some(1)"),
        (Some(("\x1b[11;3R", "\x1b[11;5R")), "Some(2)"),
        (Some(("\x1b[11;2R", "\x1b[11;5R")), "None"),
        (None, "None"),
    ] {
        let output = run_fixture(reply);
        assert!(
            output.contains(&format!("PROBE_RESULT={expected}")),
            "{output}"
        );
    }
}
