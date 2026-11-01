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
fn probe_fixture() {
    if std::env::var_os(FIXTURE).is_none() {
        return;
    }
    let mut session = TerminalSession::enter().unwrap();
    let deadline = crate::test_watchdog::Deadline::new("width probe input fixture");
    let mut keys = String::new();
    let mut paste = String::new();
    let mut responsive = false;
    let mut shift_f3 = false;
    while session.width_probe.is_some() || keys.len() < 3 || paste.is_empty() || !shift_f3 {
        assert!(!deadline.expired(), "width probe input fixture watchdog");
        session.poll_terminal_width();
        if event::poll(Duration::from_millis(5)).unwrap() {
            match event::read().unwrap() {
                Event::Key(key) => {
                    if key.code == KeyCode::F(3)
                        && key.modifiers == crossterm::event::KeyModifiers::SHIFT
                    {
                        shift_f3 = true;
                    } else if let KeyCode::Char(c) = key.code {
                        responsive |= session.width_probe.is_some();
                        keys.push(c);
                    }
                }
                Event::Paste(text) => paste.push_str(&text),
                _ => {}
            }
        }
    }
    let result = session.width_observation;
    let worker_width = std::thread::spawn(|| unicode_width::UnicodeWidthStr::width("\u{00b7}"))
        .join()
        .unwrap();
    drop(session);
    println!(
        "PROBE_RESULT={result:?};KEYS={keys};PASTE={paste};RESPONSIVE={responsive};SHIFT_F3={shift_f3};WORKER={worker_width}"
    );
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
    let mut sent = false;
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
        if !sent && output.windows(4).filter(|b| *b == b"\x1b[6n").count() >= 2 {
            let pasted = "\x1b[200~paste\u{00b7}value\x1b[201~";
            let input = if let Some((a, b)) = replies {
                format!("a\x1b[1;2R{a}{pasted}b{b}c")
            } else {
                format!("a\x1b[1;2R{pasted}bc")
            };
            master.write_all(input.as_bytes()).unwrap();
            sent = true;
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
fn probe_handles_one_two_inconsistent_and_timeout_without_losing_input() {
    for (reply, expected) in [
        (Some(("\x1b[11;2R", "\x1b[11;3R")), "One"),
        (Some(("\x1b[11;3R", "\x1b[11;5R")), "Two"),
        (Some(("\x1b[11;2R", "\x1b[11;5R")), "Unknown"),
        (Some(("\x1b[0;0R", "\x1b[11;3R")), "Unknown"),
        (None, "Unknown"),
    ] {
        let output = run_fixture(reply);
        let worker_width = if expected == "One" { 1 } else { 2 };
        assert!(
            output.contains(&format!("PROBE_RESULT=Some({expected});KEYS=abc;PASTE=paste\u{00b7}value;RESPONSIVE=true;SHIFT_F3=true;WORKER={worker_width}")),
            "{output}"
        );
    }
}
