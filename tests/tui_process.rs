#![cfg(unix)]
//! The real binary, terminal parser/renderer, local HTTP, database, and shutdown.
use serde_json::json;
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn read_until(terminal: &mut File, bytes: &mut Vec<u8>, needle: &[u8]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let mut buffer = [0; 8192];
        match terminal.read(&mut buffer) {
            Ok(count) if count > 0 => bytes.extend_from_slice(&buffer[..count]),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("terminal read: {error}"),
        }
        if bytes.windows(needle.len()).any(|slice| slice == needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "terminal did not show expected text: {}",
            String::from_utf8_lossy(bytes)
        );
        assert!(
            bytes.len() < 4 * 1024 * 1024,
            "unexpected terminal output flood"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// Strip only terminal escape sequences for observing emitted footer labels;
// this is not a screen emulator. Ratatui may style each Unicode character.
fn emitted_text(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            if bytes[i + 1] == b'[' {
                i += 2;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7e).contains(&b) {
                        break;
                    }
                }
                continue;
            }
            if bytes[i + 1] == b']' {
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 7 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            i += 2;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
async fn collect_output(terminal: &mut File, output: &mut Vec<u8>, duration: Duration) -> Vec<u8> {
    let start = output.len();
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let mut buffer = [0; 8192];
        match terminal.read(&mut buffer) {
            Ok(n) => output.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("terminal read: {e}"),
        };
        assert!(output.len() < 4 * 1024 * 1024, "terminal output flood");
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    output[start..].to_vec()
}
async fn read_until_text(terminal: &mut File, output: &mut Vec<u8>, label: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if emitted_text(output).contains(label) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "missing footer {label}: {}",
            emitted_text(output)
        );
        collect_output(terminal, output, Duration::from_millis(20)).await;
    }
}
fn color_updates(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .windows(2)
        .enumerate()
        .filter_map(|(index, prefix)| {
            if prefix != b"\x1b[" {
                return None;
            }
            let tail = &bytes[index + 2..];
            let end = tail.iter().position(|b| (0x40..=0x7e).contains(b))?;
            if tail[end] != b'm' {
                return None;
            }
            let parameters = std::str::from_utf8(&tail[..end]).ok()?;
            parameters
                .split(';')
                .filter_map(|p| p.parse::<u16>().ok())
                .any(|p| matches!(p,30..=37|38|90..=97))
                .then_some(&tail[..end])
        })
        .collect()
}
fn assert_animated_colors(bytes: &[u8], phase: &str) {
    let colors = color_updates(bytes);
    assert!(
        colors.len() >= 3,
        "{phase} footer needs repeated color updates: {:?}",
        String::from_utf8_lossy(bytes)
    );
    assert!(
        colors
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            >= 2,
        "{phase} footer must change colors rather than only emit reset codes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tui_accepts_text_renders_proxy_response_and_exits_cleanly() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='http://{}/v1'\n",listener.local_addr().unwrap())).unwrap();
    let (allow_delta, delta_ready) = tokio::sync::oneshot::channel();
    let (allow_completion, completion_ready) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        let split = loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(index) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let length = String::from_utf8_lossy(&bytes[..split])
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse::<usize>().ok())
            })
            .unwrap();
        while bytes.len() < split + length {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        let request: serde_json::Value =
            serde_json::from_slice(&bytes[split..split + length]).unwrap();
        assert_eq!(request["store"], false);
        assert!(request.to_string().contains("hello from actual terminal"));
        let code = (0..100)
            .map(|line| format!("let line_{line:03} = {line};\n"))
            .collect::<String>();
        let answer = format!("# Native scrolling\n\n```rust\n{code}```\n\nSHELLBOY_TUI_OK");
        let response = json!({"id":"tui-fixture","status":"completed","output":[{"type":"message","id":"answer","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":answer}]}]});
        let delta = format!(
            "data: {}\n\n",
            json!({"type":"response.output_text.delta","delta":answer})
        );
        let complete = format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":response})
        );
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            delta.len() + complete.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), delta_ready)
            .await
            .unwrap()
            .unwrap();
        stream.write_all(delta.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), completion_ready)
            .await
            .unwrap()
            .unwrap();
        stream.write_all(complete.as_bytes()).await.unwrap();
    });
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 28,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: pointers refer to live output descriptors and an initialized size;
    // ownership of successful returned descriptors transfers immediately to File.
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
    let mut terminal = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    // SAFETY: fcntl changes flags on this live owned pty only.
    let flags = unsafe { libc::fcntl(terminal.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe {
            libc::fcntl(
                terminal.as_raw_fd(),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            )
        },
        0
    );
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
        .arg("--config")
        .arg(&config)
        .arg("--data-dir")
        .arg(&data)
        .current_dir(root.path())
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = Vec::new();
    // F1 belongs to the normal footer. Bootstrap already offers Ctrl+Q, but
    // cannot accept conversation input or promise that IPC is ready yet.
    read_until(&mut terminal, &mut output, b"F1").await;
    terminal.write_all(b"hello from actual terminal\r").unwrap();
    read_until_text(&mut terminal, &mut output, "モデル応答待ち").await;
    let waiting = collect_output(&mut terminal, &mut output, Duration::from_millis(700)).await;
    assert_animated_colors(&waiting, "waiting");
    // Input is handled while the mocked request is deliberately stalled.
    terminal
        .write_all(b"\x1b[200~EDITABLE_WHILE_WAITING\x1b[201~")
        .unwrap();
    read_until_text(&mut terminal, &mut output, "EDITABLE_WHILE_WAITING").await;
    allow_delta.send(()).unwrap();
    read_until_text(&mut terminal, &mut output, "応答受信中").await;
    read_until_text(&mut terminal, &mut output, "SHELLBOY_TUI_OK").await;
    let receiving = collect_output(&mut terminal, &mut output, Duration::from_millis(700)).await;
    assert_animated_colors(&receiving, "receiving");
    // A real mouse event pauses on the provisional attempt before the driver
    // commits a distinct persisted assistant node. Exact row identity is
    // asserted by TestBackend; emitted PTY bytes are not a screen emulator.
    terminal.write_all(b"\x1b[<64;10;5M").unwrap();
    read_until_text(&mut terminal, &mut output, "Ctrl+Endで末尾へ").await;
    allow_completion.send(()).unwrap();
    read_until_text(&mut terminal, &mut output, "完了").await;
    collect_output(&mut terminal, &mut output, Duration::from_millis(800)).await;
    let idle = collect_output(&mut terminal, &mut output, Duration::from_millis(500)).await;
    assert!(
        idle.is_empty(),
        "completed idle must stop terminal redraw bytes: {}",
        String::from_utf8_lossy(&idle)
    );
    terminal.write_all(&[0x11]).unwrap(); // Ctrl+Q after raw mode has disabled IXON.
    // A terminal emulator keeps reading while its child shuts down. Without
    // draining the master here, a final redraw can fill the PTY buffer and
    // block the child's terminal writer before it can exit.
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        let exit = child.wait();
        tokio::pin!(exit);
        loop {
            tokio::select! {
                status = &mut exit => break status,
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    let mut buffer = [0; 8192];
                    match terminal.read(&mut buffer) {
                        Ok(count) => output.extend_from_slice(&buffer[..count]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
                            || error.raw_os_error() == Some(libc::EIO) => {},
                        Err(error) => panic!("terminal read during shutdown: {error}"),
                    }
                    assert!(output.len() < 4 * 1024 * 1024, "unexpected terminal output flood during shutdown");
                }
            }
        }
    })
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    proxy.await.unwrap();
    let db = rusqlite::Connection::open(data.join("shellboy.db")).unwrap();
    let complete: i64 = db
        .query_row(
            "SELECT count(*) FROM turn WHERE state='completed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(complete, 1);
    let response: String = db
        .query_row(
            "SELECT payload_json FROM node WHERE kind='assistant_response' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(response.contains("let line_000 = 0;"));
    assert!(response.contains("let line_099 = 99;"));
    assert!(response.contains("SHELLBOY_TUI_OK"));
    let draft: String = db
        .query_row(
            "SELECT body_json FROM draft WHERE state='editing' ORDER BY updated_at DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(draft.contains("EDITABLE_WHILE_WAITING"));
    assert!(!data.join("endpoint.json").exists());
}
