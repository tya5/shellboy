//! Disposable PTY: the real session draws before any terminal reply is sent.
use super::*;
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Command, Stdio},
};
const FIXTURE: &str = "SHELLBOY_CAPABILITY_INPUT_FIXTURE";

#[test]
fn probe_fixture() {
    if std::env::var_os(FIXTURE).is_none() {
        return;
    }
    let mut session = TerminalSession::enter().unwrap();
    session
        .terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new("FIRST_FRAME_WITHOUT_REPLIES"), frame.area());
        })
        .unwrap();
    eprintln!("FRAME_WRITTEN");
    assert_eq!(session.width_observation(), None);
    let mut typed = String::new();
    let mut pasted = String::new();
    let deadline = crate::test_watchdog::Deadline::new("terminal capability input");
    loop {
        assert!(!deadline.expired(), "input stalled: {typed:?}");
        if let Some(event) = session
            .capabilities
            .poll(&session._guard, None, Duration::from_millis(25))
            .unwrap()
        {
            match event {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    ..
                }) => break,
                Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                }) => typed.push(c),
                Event::Paste(text) => pasted.push_str(&text),
                _ => {}
            }
        }
    }
    eprintln!(
        "PROBE_RESULT={:?};KEYBOARD={};TYPED={typed:?};PASTE={pasted:?}",
        session.width_observation(),
        session.capabilities.keyboard_enabled
    );
}

fn run_fixture(replies: &[u8]) -> String {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: output descriptors become independently owned Files on success.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
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
    // SAFETY: the child owns only its disposable slave as controlling terminal.
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
    let deadline = crate::test_watchdog::Deadline::new("capability PTY child");
    let mut output = Vec::new();
    let mut sent = false;
    loop {
        let mut bytes = [0; 8192];
        match master.read(&mut bytes) {
            Ok(n) => output.extend_from_slice(&bytes[..n]),
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(libc::EIO) => {
            }
            Err(e) => panic!("PTY: {e}"),
        }
        assert!(output.len() < 128 * 1024);
        if !sent
            && output
                .windows(b"FRAME_WRITTEN".len())
                .any(|w| w == b"FRAME_WRITTEN")
        {
            assert!(
                output.windows(b"\x1b[?6n".len()).any(|w| w == b"\x1b[?6n"),
                "queries precede frame"
            );
            master.write_all(replies).unwrap();
            master.write_all(b"abq").unwrap();
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
            assert!(sent, "no frame before child exited: {text}");
            return text;
        }
        if deadline.expired() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("PTY watchdog: {}", String::from_utf8_lossy(&output));
        }
        std::thread::yield_now();
    }
}

#[test]
fn first_frame_and_input_do_not_wait_for_any_capability_reply() {
    let output = run_fixture(b"");
    assert!(
        output.contains("PROBE_RESULT=None;KEYBOARD=false;TYPED=\"ab\""),
        "{output}"
    );
}
#[test]
fn keyboard_reply_without_da_and_late_width_preserve_input() {
    let output = run_fixture(b"\x1b[?1u\x1b[?11;2R\x1b[?11;3R");
    assert!(
        output.contains("PROBE_RESULT=Some(1);KEYBOARD=true;TYPED=\"ab\""),
        "{output}"
    );
}
#[test]
fn wide_inconsistent_and_paste_observations_are_distinct() {
    for (reply, expected) in [
        (b"\x1b[?11;3R\x1b[?11;5R".as_slice(), "Some(2)"),
        (b"\x1b[?11;2R\x1b[?11;5R".as_slice(), "None"),
        (b"\x1b[200~x\x1b[?1u\x1b[201~".as_slice(), "None"),
    ] {
        let output = run_fixture(reply);
        assert!(
            output.contains(&format!(
                "PROBE_RESULT={expected};KEYBOARD=false;TYPED=\"ab\""
            )),
            "{output}"
        );
        if reply.starts_with(b"\x1b[200~") {
            assert!(output.contains("PASTE=\"x\\u{1b}[?1u\""), "{output}");
        }
    }
}
