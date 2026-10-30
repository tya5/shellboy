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
    // A terminal diff can move the cursor across unchanged blank cells rather
    // than emitting their spaces. Compare meaningful text independent of those
    // layout bytes; all words, punctuation and numeric counts must still match.
    let compact = |text: &str| {
        text.chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
    };
    let needle = compact(label);
    loop {
        if compact(&emitted_text(output)).contains(&needle) {
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
// This observer is not a VT emulator: cursor-diff bytes alone cannot recover
// reused cells. Ask the real UI for a complete repaint, then require all labels
// in that same fresh frame rather than joining unrelated historical diffs.
async fn read_repaint_text(terminal: &mut File, output: &mut Vec<u8>, labels: &[&str]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        terminal.write_all(&[0x0c]).unwrap();
        let repaint = collect_output(terminal, output, Duration::from_millis(120)).await;
        let shown = emitted_text(&repaint);
        if repaint.windows(4).any(|bytes| bytes == b"\x1b[2J")
            && labels.iter().all(|label| shown.contains(label))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "full repaint did not contain {labels:?}: {shown}"
        );
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

// Synthetic embedding endpoint: one durable authentication pause, then success.
async fn embedding_fixture() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut first = true;
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let split = loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let length = String::from_utf8_lossy(&bytes[..split])
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|n| n.trim().parse::<usize>().ok())
                })
                .unwrap();
            while bytes.len() < split + length {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&bytes[split..split + length]).unwrap();
            assert_eq!(request["model"], "embedding-fixture");
            let (code, body) = if first {
                first = false;
                (
                    401,
                    json!({"error":{"code":"invalid_api_key","message":"synthetic pause"}}),
                )
            } else {
                (200, json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}))
            };
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (url, task)
}
async fn seed_paused_history(data: &std::path::Path, source: &str) -> (String, String) {
    use sha2::{Digest, Sha256};
    use shellboy::{config::Config, index::IndexService, protocol::IndexArgs, storage::DbWorker};
    use std::sync::Arc;
    std::fs::create_dir_all(data).unwrap();
    let db = DbWorker::open(data.join("shellboy.db")).await.unwrap();
    let service = IndexService::new(db.clone(), Arc::new(Config::parse(source).unwrap()));
    let source = json!({"role":"user","content":[{"type":"input_text","text":"synthetic paused index source"}]}).to_string();
    let body_hash = Sha256::digest(source.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    db.call(move |db| {
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES('paused-source','user','2026-10-30T00:00:00Z',?1,?2,?3)", rusqlite::params![source,source.len() as i64,body_hash])?;
        Ok(())
    }).await.unwrap();
    service
        .enqueue_new(&["paused-source".into()])
        .await
        .unwrap();
    service
        .process_one(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let status = service.status(IndexArgs::default()).await.unwrap();
    assert!(status.paused);
    let paused_at = status.paused_at.unwrap();
    let thread = uuid::Uuid::new_v4().to_string();
    let target = thread.clone();
    let saved_at = paused_at.clone();
    let cwd = data.parent().unwrap().to_string_lossy().into_owned();
    let agents = data
        .parent()
        .unwrap()
        .join("AGENTS.md")
        .to_string_lossy()
        .into_owned();
    db.call(move |db| {
        let payload = json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {}",json!({"kind":"context_changed","payload":{"kind":"index_state","change":{"before":null,"after":{"paused":true,"pause_kind":"authentication","paused_at":saved_at,"pause_id":status.pause_id}}}}))}]}).to_string();
        let hash = Sha256::digest(payload.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,'Synthetic paused index',?2,?3,'fixture','fixture-model','2026-10-30T00:00:00Z')", rusqlite::params![target,cwd,agents])?;
        db.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash) VALUES('pause-history','event','2026-10-30T00:00:00Z',?1,?2,?3)",rusqlite::params![payload,payload.len() as i64,hash])?;
        db.execute("INSERT INTO thread_path(thread_id,ordinal,node_id) VALUES(?1,0,'pause-history')",[&target])?;
        db.execute("UPDATE thread SET cursor_node_id='pause-history',selection_revision=1 WHERE id=?1",[&target])?;
        Ok(())
    }).await.unwrap();
    db.shutdown().await.unwrap();
    (thread, paused_at)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tui_accepts_text_renders_proxy_response_and_exits_cleanly() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    let (embedding_url, embedding_proxy) = embedding_fixture().await;
    let config_source = format!(
        "default_connection='fixture'\ndefault_model='fixture-model'\n[connections.fixture]\nbase_url='http://{}/v1'\n[connections.embedding]\nbase_url='{embedding_url}'\n[embedding]\nconnection='embedding'\nmodel='embedding-fixture'\n",
        listener.local_addr().unwrap()
    );
    std::fs::write(&config, &config_source).unwrap();
    let (paused_thread, paused_at) = seed_paused_history(&data, &config_source).await;
    let (allow_delta, delta_ready) = tokio::sync::oneshot::channel();
    let (allow_completion, completion_ready) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
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
    let mut foreign_output = slave.try_clone().unwrap();
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
    // Load an immutable pause observation from disk, then use real management
    // dispatch to explicitly resume only its previously authorized failed work.
    terminal
        .write_all(format!("/switch {paused_thread}\r").as_bytes())
        .unwrap();
    read_until_text(&mut terminal, &mut output, "/index → T").await;
    read_until_text(&mut terminal, &mut output, &paused_at).await;
    terminal.write_all(b"/index\r").unwrap();
    read_until_text(&mut terminal, &mut output, "P 費用preview").await;
    terminal.write_all(b"p").unwrap();
    read_until_text(&mut terminal, &mut output, "対象を明示してください").await;
    terminal.write_all(b"t").unwrap();
    read_repaint_text(
        &mut terminal,
        &mut output,
        &["失敗した許可済み索引作業", "資料数: 1", "対象文字数: 29"],
    )
    .await;
    terminal.write_all(b"y").unwrap();
    collect_output(&mut terminal, &mut output, Duration::from_millis(150)).await;
    terminal.write_all(b"s").unwrap();
    read_repaint_text(&mut terminal, &mut output, &["一時停止: false"]).await;
    terminal.write_all(b"\x1b").unwrap();
    collect_output(&mut terminal, &mut output, Duration::from_millis(100)).await;
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
    // An external terminal writer can invalidate the app's remembered screen.
    foreign_output
        .write_all(b"\x1b[3;79HFOREIGN_SCREEN_MARKER")
        .unwrap();
    let foreign = collect_output(&mut terminal, &mut output, Duration::from_millis(100)).await;
    assert!(
        foreign
            .windows(b"FOREIGN_SCREEN_MARKER".len())
            .any(|v| v == b"FOREIGN_SCREEN_MARKER")
    );
    terminal.write_all(&[0x0c]).unwrap(); // Ctrl+L
    let repaint = collect_output(&mut terminal, &mut output, Duration::from_millis(350)).await;
    assert!(
        repaint.windows(4).any(|v| v == b"\x1b[2J"),
        "manual repaint must clear the real terminal"
    );
    let text = emitted_text(&repaint);
    assert!(
        text.contains("EDITABLE_WHILE_WAITING"),
        "draft must survive repaint: {text}"
    );
    assert!(
        text.contains("Ctrl+Endで末尾へ"),
        "scroll anchor/follow must survive repaint: {text}"
    );
    assert!(!text.contains("FOREIGN_SCREEN_MARKER"));
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
    let pauses: i64 = db
        .query_row(
            "SELECT count(*) FROM app_meta WHERE key LIKE 'index.pause.%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pauses, 0, "explicit retry clears the saved pause");
    embedding_proxy.abort();
    let _ = embedding_proxy.await;
}
