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
    let binary = test_binary();
    let owner = crate::pty_session::ServeFixture::start(&binary, root.path(), &config, &data).await;
    let (mut child, mut terminal) = launch_client(&binary, root.path(), &config, &data);
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

fn test_binary() -> std::ffi::OsString {
    std::env::var_os("SHELLBOY_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_shellboy").into())
}

fn launch_client(
    binary: &std::ffi::OsStr,
    cwd: &std::path::Path,
    config: &std::path::Path,
    data: &std::path::Path,
) -> (tokio::process::Child, pty_screen::Pty) {
    let (terminal, slave) = pty();
    let mut command = tokio::process::Command::new(binary);
    crate::pty_session::isolate_child(&mut command);
    let child = command
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .current_dir(cwd)
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    (child, pty_screen::Pty::new(terminal, 28, 100))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relaunched_native_client_restores_the_owner_saved_draft() {
    use shellboy::protocol::{Command, DraftGetArgs, ThreadCreateArgs};

    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    std::fs::write(
        &config,
        "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n",
    ).unwrap();
    let binary = test_binary();
    let _owner =
        crate::pty_session::ServeFixture::start(&binary, root.path(), &config, &data).await;
    let response = shellboy::ipc::call(
        &data,
        Command::ThreadCreate(ThreadCreateArgs {
            agents: None,
            cwd: None,
            parent_thread_id: None,
            connection: None,
            model: None,
            effort: None,
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(response.ok, "{:?}", response.error);
    let result = response.result.unwrap();
    let thread = result["thread_id"].as_str().unwrap();
    let title = "Relaunch draft fixture";
    let set = serde_json::from_value(serde_json::json!({
        "command": "thread.set", "args": {"thread_id": thread, "title": title}
    }))
    .unwrap();
    let response = shellboy::ipc::call(&data, set, None, None).await.unwrap();
    assert!(response.ok, "{:?}", response.error);

    let select = format!("/switch {thread}\r");
    let draft = "draft survives client relaunch";
    let (mut child, mut terminal) = launch_client(&binary, root.path(), &config, &data);
    terminal.until_screen(&["F1"]).await;
    terminal.send(select.as_bytes());
    terminal.until_screen(&[title]).await;
    terminal.send(draft.as_bytes());
    terminal.until_screen(&[draft]).await;
    terminal.send(&[0x11]);
    assert!(terminal.until_exit(&mut child).await.success());
    let saved = shellboy::ipc::call(
        &data,
        Command::DraftGet(DraftGetArgs {
            thread_id: Some(thread.to_owned()),
            operation_id: None,
        }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(saved.ok, "{:?}", saved.error);
    let saved = saved.result.unwrap();
    assert_eq!(saved["draft"]["text"], draft);
    assert!(saved["view"].is_object());

    let (mut child, mut terminal) = launch_client(&binary, root.path(), &config, &data);
    terminal.until_screen(&["F1"]).await;
    terminal.send(select.as_bytes());
    terminal.until_screen(&[draft]).await;
    terminal.send(&[0x11]);
    assert!(terminal.until_exit(&mut child).await.success());
}
