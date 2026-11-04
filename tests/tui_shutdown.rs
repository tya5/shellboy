#![cfg(unix)]
//! A real process must restore the terminal and exit even when its DB worker
//! cannot finish. The injected recursive query exists only in a temporary DB.
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
async fn explicit_force_exits_a_blocked_db_worker_and_restores_terminal_modes() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    std::fs::write(&config, "[io]\nshutdown_grace_ms=50\n").unwrap();
    let (terminal, slave) = pty();
    // A copied binary keeps other shared-target builds from changing the child.
    let binary = std::env::var_os("SHELLBOY_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_shellboy").into());
    let mut command = tokio::process::Command::new(binary);
    // Size/input APIs may prefer /dev/tty over stdio. Give this child only the
    // disposable PTY, never the parent's interactive controlling terminal.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1
                || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
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
    // Bootstrap can render Ctrl+Q before it publishes the endpoint. Wait for
    // normal conversation controls in a completed frame.
    terminal.until_screen(&["F1"]).await;
    let old_endpoint: shellboy::app::Endpoint =
        serde_json::from_slice(&std::fs::read(data.join("endpoint.json")).unwrap()).unwrap();
    let db = rusqlite::Connection::open(data.join("shellboy.db")).unwrap();
    db.busy_timeout(Duration::from_secs(2)).unwrap();
    // No request/side effect is started. Force exit interrupts this deliberately
    // expensive DB instruction after the UI grace/confirmation, not a sleep.
    db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('force-thread','/tmp','/tmp/AGENTS.md','fixture','fixture','now'); INSERT INTO run(id,thread_id,state,created_at) VALUES('force-run','force-thread','live','now'); CREATE TRIGGER fixture_block_shutdown BEFORE UPDATE OF state ON run WHEN OLD.id='force-run' BEGIN SELECT (WITH RECURSIVE counter(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM counter WHERE x<1000000000) SELECT sum(x) FROM counter); END;").unwrap();
    eprintln!("shutdown phase: Ctrl+Q to initial graceful wait");
    terminal.send(&[0x11]);
    terminal.until_screen(&["W/Enter:"]).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "Grace must not silently force exit"
    );
    for return_key in [Some(b'n'), Some(0x1b), None] {
        eprintln!("shutdown phase: force confirmation, return={return_key:?}");
        terminal.send(b"f");
        terminal
            .until_screen(&["N / Esc Back", "Y Force quit"])
            .await;
        assert!(
            child.try_wait().unwrap().is_none(),
            "Force requires its second confirmation"
        );
        if let Some(return_key) = return_key {
            terminal.send(&[return_key]);
            terminal.until_screen(&["W/Enter:"]).await;
            assert!(
                child.try_wait().unwrap().is_none(),
                "N and Esc must return to graceful waiting without forcing exit"
            );
        }
    }
    let restoration_start = terminal.screen.raw.len();
    terminal.send(b"y");
    // Keep consuming the PTY while the child restores modes. A real terminal
    // continuously drains output; waiting for exit first can lose the Darwin
    // PTY's queued bytes when its final slave descriptor closes.
    terminal.until_raw(b"Forced exit:").await;
    let status = terminal.until_exit(&mut child).await;
    assert_eq!(status.code(), Some(1));
    let output = &terminal.screen.raw[restoration_start..];
    for sequence in [b"\x1b[?1049l".as_slice(), b"\x1b[?2004l".as_slice()] {
        assert!(
            output.windows(sequence.len()).any(|v| v == sequence),
            "Terminal mode was not restored: missing {:?} in {:?}",
            String::from_utf8_lossy(sequence),
            String::from_utf8_lossy(output)
        );
    }
    assert!(String::from_utf8_lossy(output).contains("Forced exit:"));
    // IPC cleanup may have completed before the blocked DB stage. Remove any
    // remaining socket published by our dead child, never a PID-based target.
    let old_socket = std::path::PathBuf::from(old_endpoint.address);
    for result in [
        std::fs::remove_file(&old_socket),
        std::fs::remove_dir(old_socket.parent().unwrap()),
    ] {
        if let Err(error) = result {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "Unexpected IPC fixture cleanup error: {error}"
            );
        }
    }
    // The unfinished transaction rolled back. Remove the test-only blocker
    // before normal recovery and prove this old Run is never auto-resumed.
    db.execute_batch("DROP TRIGGER fixture_block_shutdown;")
        .unwrap();
    assert_eq!(
        db.query_row("SELECT state FROM run WHERE id='force-run'", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "live"
    );
    db.close().unwrap();
    let owner = shellboy::app::DataOwner::start(Some(config), Some(data), root.path().into())
        .await
        .unwrap();
    owner
        .db
        .call(|db| {
            let (state, reason): (String, Option<String>) = db.query_row(
                "SELECT state,stop_reason FROM run WHERE id='force-run'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(state, "stopped");
            assert!(reason.unwrap().contains("previous_instance"));
            Ok(())
        })
        .await
        .unwrap();
    let (send, _receive) = tokio::sync::mpsc::channel(4);
    let server = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    server.shutdown().await.unwrap();
    owner.shutdown().await.unwrap();
}
