#![cfg(unix)]
//! A real attached client must restore the terminal without draining its owner.
use std::{
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    time::Duration,
};

fn pty() -> (File, File) {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 28,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: valid initialized pointers; successful descriptors transfer to File.
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
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    // SAFETY: fcntl accesses our live owned master descriptor only.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    (master, slave)
}
#[path = "support/pty_screen.rs"]
mod pty_screen;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_quit_restores_terminal_while_owner_storage_is_locked() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    std::fs::write(&config, "").unwrap();
    let (terminal, slave) = pty();
    let binary = std::env::var_os("SHELLBOY_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_shellboy").into());
    let owner = crate::pty_session::ServeFixture::start(&binary, root.path(), &config, &data).await;
    let mut command = tokio::process::Command::new(binary);
    crate::pty_session::isolate_child(&mut command);
    let mut child = command
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .current_dir(root.path())
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut terminal = pty_screen::Pty::new(terminal, 28, 100);
    terminal.until_screen(&["F1"]).await;
    let db = rusqlite::Connection::open(data.join("shellboy.db")).unwrap();
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let restoration_start = terminal.screen.raw.len();
    terminal.send(&[0x11]);
    terminal.until_raw(b"\x1b[?1049l").await;
    assert!(terminal.until_exit(&mut child).await.success());
    assert!(
        terminal.screen.raw[restoration_start..]
            .windows(8)
            .any(|v| v == b"\x1b[?2004l")
    );
    db.execute_batch("ROLLBACK").unwrap();
    let response = shellboy::ipc::call(
        &data,
        shellboy::protocol::Command::ThreadList(Default::default()),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(response.ok, "{:?}", response.error);
    drop(owner);
}
