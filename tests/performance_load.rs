#![cfg(unix)]
//! Explicit release-only real-binary load evidence, synthetic localhost data only.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const SAMPLES: usize = 14;
fn command(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn load() -> [f64; 3] {
    let mut values = [0.; 3];
    assert_eq!(unsafe { libc::getloadavg(values.as_mut_ptr(), 3) }, 3);
    values
}
// Streaming ANSI removal preserves split UTF-8 bytes and escape sequences. It
// detects emitted text, not an emulator's pixels, cursor positioning or IME.
#[derive(Default)]
struct Output {
    state: u8,
    visible: Vec<u8>,
    bytes: usize,
}
impl Output {
    fn clear(&mut self) {
        self.visible.clear();
        self.bytes = 0;
    }
    fn push(&mut self, bytes: &[u8]) {
        self.bytes += bytes.len();
        assert!(self.bytes <= (4 * 1024 * 1024), "terminal output flood");
        for &byte in bytes {
            self.state = match self.state {
                0 if byte == 0x1b => 1,
                0 => {
                    self.visible.push(byte);
                    0
                }
                1 if byte == b'[' => 2,
                1 if matches!(byte, b']' | b'P' | b'X' | b'^' | b'_') => 3,
                1 if (0x20..=0x2f).contains(&byte) => 5,
                1 => 0,
                2 if (0x40..=0x7e).contains(&byte) => 0,
                2 => 2,
                3 if byte == 7 => 0,
                3 if byte == 0x1b => 4,
                3 => 3,
                4 if byte == b'\\' => 0,
                4 => 3,
                5 if (0x30..=0x7e).contains(&byte) => 0,
                _ => 5,
            };
        }
    }
    fn contains(&self, needle: &[u8]) -> bool {
        self.visible
            .windows(needle.len())
            .any(|part| part == needle)
    }
}
struct Tui {
    child: Child,
    terminal: File,
    output: Output,
    last_write: Duration,
}
impl Drop for Tui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            // Even failed assertions must ask the owner to stop its shell groups.
            let _ = self.terminal.write_all(&[0x11]);
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                let mut bytes = [0; 8192];
                let _ = self.terminal.read(&mut bytes);
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl Tui {
    fn start(binary: &Path, config: &Path, data: &Path, cwd: &Path) -> (Self, Duration) {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 32,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid output pointers and size; File takes ownership once.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0
        );
        let terminal = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
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
        let start = Instant::now();
        let child = Command::new(binary)
            .arg("--config")
            .arg(config)
            .arg("--data-dir")
            .arg(data)
            .current_dir(cwd)
            .env("TERM", "xterm-256color")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave)
            .spawn()
            .unwrap();
        let mut tui = Self {
            child,
            terminal,
            output: Output::default(),
            last_write: Duration::ZERO,
        };
        // Measure readiness for conversation input, after the bootstrap screen.
        tui.until(b"F1");
        let elapsed = start.elapsed();
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        // Raw mode must be active: the paste must not be measured as kernel echo.
        assert_eq!(
            unsafe { libc::tcgetattr(tui.terminal.as_raw_fd(), attributes.as_mut_ptr()) },
            0
        );
        assert_eq!(unsafe { attributes.assume_init() }.c_lflag & libc::ECHO, 0);
        (tui, elapsed)
    }
    fn drain(&mut self) {
        loop {
            let mut bytes = [0; 8192];
            match self.terminal.read(&mut bytes) {
                Ok(0) => break,
                Ok(read) => self.output.push(&bytes[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("PTY read failed: {error}"),
            }
        }
    }
    fn until(&mut self, needle: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.drain();
            if self.output.contains(needle) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "TUI exited before measured output"
            );
            assert!(
                Instant::now() < deadline,
                "TUI output deadline exceeded for {:?}; tail {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(
                    &self.output.visible[self.output.visible.len().saturating_sub(4096)..]
                )
            );
            let mut descriptor = libc::pollfd {
                fd: self.terminal.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut descriptor, 1, 5) };
            assert!(
                result >= 0
                    || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            );
        }
    }

    fn write(&mut self, mut bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !bytes.is_empty() {
            self.drain();
            match self.terminal.write(&bytes[..bytes.len().min(4096)]) {
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(e) => panic!("PTY write: {e}"),
            };
            assert!(Instant::now() < deadline, "PTY write timeout");
        }
    }
    fn measured(&mut self, input: &[u8], needle: &str) -> Duration {
        self.drain();
        self.output.clear();
        let start = Instant::now();
        self.write(input);
        self.last_write = start.elapsed();
        self.until(needle.as_bytes());
        start.elapsed()
    }
    fn fresh_input(&mut self) {
        self.write(&[0x0e]);
        std::thread::sleep(Duration::from_millis(50));
        self.drain();
        self.output.clear();
    }
    fn quit(&mut self) -> Duration {
        self.output.clear();
        let start = Instant::now();
        self.write(&[0x11]);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut force_offer = false;
        let mut wait_sent = false;
        let mut wait_ack = false;
        loop {
            self.drain();
            if !wait_sent && self.output.contains(b"W/Enter") {
                force_offer = true;
                self.output.clear();
                self.write(b"w");
                wait_sent = true;
            }
            if wait_sent && self.output.contains("正常終了".as_bytes()) {
                wait_ack = true;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                println!(
                    "PERF_LOAD_SHUTDOWN {}",
                    json!({"elapsed_ms":start.elapsed().as_secs_f64()*1000.,"force_offer_observed":force_offer,"wait_sent":wait_sent,"wait_ack":wait_ack})
                );
                assert!(
                    !force_offer || wait_ack,
                    "grace appeared but wait input produced no acknowledgement before normal exit"
                );
                return start.elapsed();
            }
            assert!(
                Instant::now() < deadline,
                "normal shutdown exceeded fixture deadline; PTY tail {:?}",
                String::from_utf8_lossy(
                    &self.output.visible[self.output.visible.len().saturating_sub(4096)..]
                )
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

struct Proxy {
    url: String,
    embeddings: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl Proxy {
    fn start(raw_mib: usize) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let embeddings = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let counts = embeddings.clone();
        let running = active.clone();
        let token = stop.clone();
        let responses = Arc::new(AtomicUsize::new(0));
        let task = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !token.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let counts = counts.clone();
                        let running = running.clone();
                        let responses = responses.clone();
                        workers.push(std::thread::spawn(move||{
            stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();let mut bytes=Vec::new();let (split,length)=loop {let mut b=[0;8192];let n=stream.read(&mut b).unwrap_or(0);if n==0{return;}bytes.extend_from_slice(&b[..n]);if let Some(p)=bytes.windows(4).position(|v|v==b"\r\n\r\n"){let head=String::from_utf8_lossy(&bytes[..p]);let length=head.lines().find_map(|line|line.to_lowercase().strip_prefix("content-length:").and_then(|s|s.trim().parse::<usize>().ok())).unwrap_or(0);break(p+4,length);}};
            while bytes.len()<split+length {let mut b=[0;8192];let n=stream.read(&mut b).unwrap_or(0);if n==0{return;}bytes.extend_from_slice(&b[..n]);assert!(bytes.len()<8*1024*1024,"unexpected fixture request size");}
            let request:Value=serde_json::from_slice(&bytes[split..split+length]).unwrap();
            let embedding=String::from_utf8_lossy(&bytes[..split]).contains("/embeddings");
            let line=String::from_utf8_lossy(&bytes[..split]).lines().next().unwrap().to_owned();println!("PERF_LOAD_PROXY {}",json!({"line":line,"request_bytes":length}));
            let body=if line.contains("/models ") {json!({"object":"list","data":[{"id":"fixture","object":"model"}]})}else if embedding {running.fetch_add(1,Ordering::SeqCst);counts.fetch_add(1,Ordering::SeqCst);std::thread::sleep(Duration::from_millis(250));running.fetch_sub(1,Ordering::SeqCst);let count=request["input"].as_array().unwrap().len();json!({"data":(0..count).map(|i|json!({"index":i,"embedding":[1.0,0.0,0.0]})).collect::<Vec<_>>()})}
            else if responses.fetch_add(1,Ordering::SeqCst)==0 {let delay=if raw_mib==16{"0.10"}else{"0.025"};let command=format!("chunk=$(dd if=/dev/zero bs=65536 count=1 2>/dev/null | tr '\\000' x); i=0; while [ \"$i\" -lt {} ]; do printf '%s' \"$chunk\"; sleep {delay}; i=$((i+1)); done",raw_mib*16);json!({"id":"load-shell","status":"completed","output":[{"id":"load-item","type":"function_call","call_id":"load-call","name":"shell","arguments":json!({"command":command,"background":false}).to_string()}]})}
            else {json!({"id":"load-finished","status":"completed","output":[{"id":"load-answer","type":"message","role":"assistant","content":[{"type":"output_text","text":"LOAD_FINISHED"}]}]})};
            let body=body.to_string();let header=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());let _=stream.write_all(header.as_bytes());let _=stream.write_all(body.as_bytes());
        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("proxy: {e}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            embeddings,
            active,
            stop,
            task: Some(task),
        }
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let result = task.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn cli(binary: &Path, config: &Path, data: &Path, args: &[&str]) -> Value {
    let output = Command::new(binary)
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true, "{value}");
    value["result"].clone()
}
fn read_db(path: &Path) -> rusqlite::Connection {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db
}
fn snapshot(db: &rusqlite::Connection) -> Value {
    let(raw,seq):(i64,i64)=db.query_row("SELECT COALESCE(SUM(length(o.bytes)),0),COALESCE(MAX(s.saved_sequence),0) FROM output_chunk o JOIN shell_execution s ON s.id=o.execution_id WHERE s.id NOT LIKE 'gc-%'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let expired: i64 = db
        .query_row(
            "SELECT count(*) FROM output_chunk WHERE execution_id LIKE 'gc-%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    json!({"raw_bytes":raw,"saved_sequence":seq,"gc_chunks_remaining":expired})
}
fn seed(data: &Path, config: &Path, cwd: &Path, raw: bool) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let owner=shellboy::app::DataOwner::start(Some(config.into()),Some(data.into()),cwd.into()).await.unwrap();
        let cwd=cwd.to_string_lossy().into_owned();owner.db.call(move|db|{let tx=db.transaction()?;
            for thread in (0..SAMPLES).map(|i|format!("p{i}")).chain(["index-fixture".into(),"gc-fixture".into()]) {
                tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?2,?3,'fixture','fixture','2026-10-24T00:00:00Z')",rusqlite::params![thread,cwd,format!("{cwd}/AGENTS.md")])?;
                let count=if thread=="index-fixture"{100}else{50};for i in 0..count {let node=format!("{thread}-{i}");let text=if i+1==count{format!("HISTORY_{thread}_READY")}else{format!("synthetic retained node {i} {}","bounded text ".repeat(8))};let body=json!({"text":text}).to_string();let body_hash=hash(body.as_bytes());tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes,body_hash)VALUES(?1,'assistant','2026-10-24T00:00:00Z',?2,?3,?4)",rusqlite::params![node,body,body.len() as i64,body_hash])?;tx.execute("INSERT INTO thread_path(thread_id,ordinal,node_id)VALUES(?1,?2,?3)",rusqlite::params![thread,i,node])?;}
                tx.execute("UPDATE thread SET cursor_node_id=?2 WHERE id=?1",rusqlite::params![thread,format!("{thread}-{}",count-1)])?;
            }
            if raw {tx.execute_batch("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('gc-run','gc-fixture','completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z');INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('gc-turn','gc-fixture','gc-run','completed','2020-01-01T00:00:00Z','2020-01-01T00:00:01Z');")?;
                for i in 0..128 {let id=format!("gc-{i}");tx.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,saved_sequence,exit_code,output_complete,started_at,ended_at)VALUES(?1,'gc-run','gc-turn',?1,'exited','/','{}','UTF-8','UTF-8','closed',1,0,1,'2020-01-01T00:00:00Z','2020-01-01T00:00:01Z')",[&id])?;tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES(?1,0,'stdout',0,zeroblob(524288),'2020-01-01T00:00:00Z')",[id])?;}
            }tx.commit()?;Ok(())}).await.unwrap();owner.shutdown().await.unwrap();
    });
}
fn report(metric: &str, phase: usize, values: &[f64]) {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let p = |rank: f64| v[((rank * v.len() as f64).ceil() as usize).saturating_sub(1)];
    println!(
        "PERF_LOAD_RESULT {}",
        json!({"metric":metric,"raw_mib":phase,"unit":"ms","n":v.len(),"p50":p(0.5),"p95":p(0.95),"max":v.last(),"samples":values})
    );
}
#[test]
#[ignore = "opt-in release actual PTY, 16/64MiB shell + synthetic index/GC; ~2 minutes"]
#[allow(clippy::assertions_on_constants)] // The ignored debug harness must compile but must not supply performance evidence.
fn real_binary_large_paste_switch_under_raw_index_gc_load() {
    assert!(
        !cfg!(debug_assertions),
        "measure explicitly with cargo test --release"
    );
    let binary = std::env::var_os("SHELLBOY_PERF_LOAD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_shellboy").into());
    println!(
        "PERF_LOAD_ENV {}",
        json!({"commit":command("git",&["rev-parse","HEAD"]),"binary_sha256":hash(&std::fs::read(&binary).unwrap()),"os":command("uname",&["-srm"]),"rustc":command("rustc",&["--version"]),"load":load(),"pty":[120,32],"fps":30,"draft_limit_bytes":1048576,"shell_raw_cap_bytes":67108864,"body_cache_bytes":1048576,"history_cache_bytes":1048576,"stream_buffer_bytes":8388608,"index_split_chars":2000000,"raw_burst_bytes":65536,"raw16_sleep_ms":100,"raw64_sleep_ms":25,"embedding_delay_ms":250,"fixture_nodes":850,"samples":SAMPLES,"boundary":"before first PTY input write through observed ANSI-stripped emitted text; includes bounded nonblocking write; no pixels/IME"})
    );
    let selected = std::env::var("SHELLBOY_PERF_LOAD_MIB").ok().map(|value| {
        value
            .parse::<usize>()
            .expect("SHELLBOY_PERF_LOAD_MIB must be 0,16,64")
    });
    assert!(selected.is_none_or(|value| [0, 16, 64].contains(&value)));
    for raw_mib in [0, 16, 64]
        .into_iter()
        .filter(|value| selected.is_none_or(|selected| selected == *value))
    {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        let data = root.path().join("data");
        let proxy = Proxy::start(raw_mib);
        let embedding = if raw_mib > 0 {
            "[embedding]\nconnection='fixture'\nmodel='synthetic-3d'\n"
        } else {
            ""
        };
        std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='{}'\n{embedding}[cache]\nbody_bytes=1048576\nhistory_bytes=1048576\n[indexing]\nsplit_chars=2000000\n[io]\nterm_grace_ms=100\ndrain_timeout_ms=100\n",proxy.url)).unwrap();
        seed(&data, &config, root.path(), raw_mib > 0);
        let (mut tui, _) = Tui::start(&binary, &config, &data, root.path());
        let db_path = data.join("shellboy.db");
        let db = read_db(&db_path);
        let mut gc = None;
        if raw_mib > 0 {
            let before = snapshot(&db);
            if before["gc_chunks_remaining"].as_i64().unwrap() > 0 {
                let paste = tui.measured(
                    format!("\x1b[200~{}\x1b[201~", "g".repeat(65536)).as_bytes(),
                    "65536文字",
                );
                println!(
                    "PERF_LOAD_GC_PRELUDE {}",
                    json!({"raw_mib":raw_mib,"paste_bytes":65536,"paste_ms":paste.as_secs_f64()*1000.,"write_ms":tui.last_write.as_secs_f64()*1000.,"postwrite_ms":paste.saturating_sub(tui.last_write).as_secs_f64()*1000.,"before":before,"after":snapshot(&db)})
                );
                tui.fresh_input();
            } else {
                println!(
                    "PERF_LOAD_GC_PRELUDE {}",
                    json!({"raw_mib":raw_mib,"skipped":"startup GC had already removed all expired chunks"})
                );
            }

            let created = cli(&binary, &config, &data, &["thread", "create"]);
            let thread = created["thread_id"].as_str().unwrap();
            cli(
                &binary,
                &config,
                &data,
                &[
                    "message",
                    "send",
                    "--thread",
                    thread,
                    "--text",
                    "generate fixture raw output",
                ],
            );
            cli(
                &binary,
                &config,
                &data,
                &[
                    "index",
                    "backfill",
                    "--scope",
                    "threads",
                    "--thread",
                    "index-fixture",
                ],
            );
            gc = Some(
                Command::new(&binary)
                    .arg("--config")
                    .arg(&config)
                    .arg("--data-dir")
                    .arg(&data)
                    .args(["storage", "collect", "--category", "raw"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while snapshot(&db)["raw_bytes"].as_i64().unwrap() == 0 {
                tui.drain();
                assert!(
                    Instant::now() < deadline,
                    "shell never produced retained raw; model attempts: {:?}",
                    db.prepare("SELECT state,error_json FROM model_attempt")
                        .unwrap()
                        .query_map([], |r| Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?
                        )))
                        .unwrap()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let mut switches = Vec::new();
        let mut small = Vec::new();
        let mut large = Vec::new();
        let mut originals = Vec::new();
        for sample in 0..SAMPLES {
            tui.fresh_input();
            let thread = format!("p{sample}");
            let before = snapshot(&db);
            let in_flight = proxy.active.load(Ordering::SeqCst);
            let switch = tui.measured(
                format!("/switch {thread}\r").as_bytes(),
                &format!("HISTORY_{thread}_READY"),
            );
            let switch_write = tui.last_write;
            switches.push(switch.as_secs_f64() * 1000.);
            let size = if sample % 2 == 0 {
                64 * 1024
            } else {
                512 * 1024
            };
            let original = format!("{}\n終", "x".repeat(size - 4));
            let characters = original.chars().count();
            let payload = format!("\x1b[200~{original}\x1b[201~");
            let paste = tui.measured(payload.as_bytes(), &format!("{characters}文字"));
            if size == 64 * 1024 {
                small.push(paste.as_secs_f64() * 1000.);
            } else {
                large.push(paste.as_secs_f64() * 1000.);
            }
            println!(
                "PERF_LOAD_SAMPLE {}",
                json!({"raw_mib":raw_mib,"sample":sample,"paste_bytes":size,"switch_ms":switch.as_secs_f64()*1000.,"paste_ms":paste.as_secs_f64()*1000.,"switch_write_ms":switch_write.as_secs_f64()*1000.,"paste_write_ms":tui.last_write.as_secs_f64()*1000.,"paste_after_write_ms":paste.saturating_sub(tui.last_write).as_secs_f64()*1000.,"before":before,"after":snapshot(&db),"embedding_active_before":in_flight,"embedding_requests_after":proxy.embeddings.load(Ordering::SeqCst),"gc_command_pending_after":gc.as_mut().is_some_and(|child|child.try_wait().unwrap().is_none()),"rss_kib":command("/bin/ps",&["-p",&tui.child.id().to_string(),"-o","rss="]),"load":load()})
            );
            originals.push((thread, original));
        }
        tui.fresh_input();
        let over_limit = format!("\x1b[200~{}\x1b[201~", "z".repeat(1048577));
        tui.measured(over_limit.as_bytes(), "貼付けを追加していません");
        tui.fresh_input();
        tui.measured(b"/switch p12\r\x0e/switch p0\r", "HISTORY_p0_READY");
        if raw_mib > 0 {
            let deadline = Instant::now() + Duration::from_secs(90);
            loop {
                let done:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution WHERE id NOT LIKE 'gc-%' AND state='exited' AND output_complete=1)",[],|r|r.get(0)).unwrap();
                if done {
                    break;
                }
                tui.drain();
                assert!(Instant::now() < deadline, "raw completion timeout");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(snapshot(&db)["raw_bytes"], raw_mib * 1024 * 1024);
        }
        let shutdown = tui.quit();
        drop(db);
        if let Some(mut child) = gc {
            assert!(child.wait().unwrap().success());
        }
        let db = read_db(&db_path);
        for (thread, original) in originals {
            let body:String=db.query_row("SELECT body_json FROM draft WHERE thread_id=?1 ORDER BY updated_at DESC LIMIT 1",[thread],|r|r.get(0)).unwrap();
            let saved: shellboy::ui::DraftSnapshot = serde_json::from_str(&body).unwrap();
            assert_eq!(saved.text, original);
            assert_eq!(saved.folds.len(), 1);
            assert!(!saved.folds[0].expanded);
        }
        let sent: i64 = db
            .query_row("SELECT count(*) FROM node WHERE kind='user'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            sent,
            if raw_mib > 0 { 1 } else { 0 },
            "pastes must remain unsent"
        );
        println!(
            "PERF_LOAD_PHASE_END {}",
            json!({"raw_mib":raw_mib,"snapshot":snapshot(&db),"embedding_requests":proxy.embeddings.load(Ordering::SeqCst),"unsent_drafts_verified":SAMPLES,"shutdown_ms":shutdown.as_secs_f64()*1000.,"load":load()})
        );
        report("switch", raw_mib, &switches);
        report("paste64KiB", raw_mib, &small);
        report("paste512KiB", raw_mib, &large);
    }
}
