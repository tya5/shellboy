#![cfg(unix)]
//! Real owner binary, PTY and native shell; all requests stay on loopback.
use serde_json::{Value, json};
use shellboy::{ipc, protocol::*};
use std::{
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{sleep, timeout},
};

struct Owner {
    child: Child,
    terminal: Option<File>,
    native_group: Option<i32>,
    wrapped_owner: Option<i32>,
    terminal_tail: Vec<u8>,
    finished: bool,
}
impl Owner {
    fn drain(&mut self) {
        if let Some(master) = &mut self.terminal {
            for _ in 0..16 {
                let mut bytes = [0; 8192];
                match master.read(&mut bytes) {
                    Ok(n) if n > 0 => {
                        self.terminal_tail.extend_from_slice(&bytes[..n]);
                        if self.terminal_tail.len() > 8192 {
                            self.terminal_tail.drain(..self.terminal_tail.len() - 8192);
                        }
                    }
                    _ => break,
                }
            }
        }
    }
    fn signal(&self, signal: i32) {
        assert_eq!(
            unsafe { libc::kill(self.wrapped_owner.unwrap_or(self.child.id() as i32), signal) },
            0,
            "send signal to owned fixture process"
        );
    }
    async fn exited(&mut self, budget: Duration) -> ExitStatus {
        timeout(budget, async {
            loop {
                self.drain();
                if let Some(status) = self.child.try_wait().unwrap() {
                    self.finished = true;
                    return status;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("owner shutdown watchdog")
    }
}
impl Owner {
    fn terminate(&mut self) {
        if let Some(owner) = self.wrapped_owner.take() {
            unsafe {
                libc::kill(owner, libc::SIGKILL);
            }
        }
        if !self.finished {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.wait();
            self.finished = true;
        }
        if let Some(group) = self.native_group.take()
            && unsafe { libc::getpgid(group) } == group
        {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.terminate();
    }
}
fn pty_owner(root: &Path, config: &Path, wrapper: Option<&str>, controlling: bool) -> Owner {
    // Test-only override lets the same physical PTY fixture compare a saved
    // baseline binary without changing production configuration or behavior.
    let binary = std::env::var_os("SHELLBOY_SIGNAL_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_shellboy")));
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 35,
        ws_col: 110,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
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
    // The child must not inherit a spare master descriptor: that would keep the
    // terminal alive after the test closes its only master.
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut command = if let Some(wrapper) = wrapper {
        let quote = |v: &Path| format!("'{}'", v.to_string_lossy().replace('\'', "'\\''"));
        let mut command = std::process::Command::new(wrapper);
        if wrapper.ends_with("bash") {
            command.args(["--noprofile", "--norc"]);
        } else {
            command.arg("-f");
        }
        command.args(["-i", "-c"]).arg(format!(
            "{} --config {} --data-dir {}; fixture_status=$?; exit \"$fixture_status\"",
            quote(&binary),
            quote(config),
            quote(&root.join("data"))
        ));
        command
    } else {
        let mut command = std::process::Command::new(&binary);
        command
            .arg("--config")
            .arg(config)
            .arg("--data-dir")
            .arg(root.join("data"));
        command
    };
    command
        .current_dir(root)
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(Stdio::from(slave));
    // Establish a real controlling terminal. Closing its master produces the
    // actual terminal-loss/HUP path rather than merely an injected draw error.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if controlling && libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Owner {
        child: command.spawn().unwrap(),
        terminal: Some(master),
        native_group: None,
        wrapped_owner: None,
        terminal_tail: Vec::new(),
        finished: false,
    }
}
struct Fixture {
    root: tempfile::TempDir,
    owner: Owner,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.owner.terminate();
        self.server.abort();
    }
}
impl Fixture {
    async fn running() -> Self {
        Self::running_with(None, false, true, 2000).await
    }
    async fn running_with(
        wrapper: Option<&str>,
        ignore_term: bool,
        controlling: bool,
        term_grace_ms: u64,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pidfile = root.path().join("native-pid");
        let quoted = format!("'{}'", pidfile.to_string_lossy().replace('\'', "'\\''"));
        let owner_pidfile = root.path().join("owner-pid");
        let owner_quoted = format!(
            "'{}'",
            owner_pidfile.to_string_lossy().replace('\'', "'\\''")
        );
        let workload = if ignore_term {
            "trap '' TERM; while :; do sleep 1; done"
        } else {
            "read fixture_input"
        };
        let shell = format!(
            "printf '%s\\n' \"$$\" > {quoted}; printf '%s\\n' \"$PPID\" > {owner_quoted}; {prefix}printf SIGNAL_SHELL_READY; {workload}",
            prefix = if ignore_term { "trap '' TERM; " } else { "" }
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.set_nodelay(true).unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 65536];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let len = String::from_utf8_lossy(&bytes[..at])
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= at + 4 + len {
                        break;
                    }
                }
            }
            let body=json!({"id":"signal-shell","status":"completed","output":[{"type":"function_call","call_id":"signal-call","name":"shell","arguments":json!({"command":shell,"stdin_open":true}).to_string()}]}).to_string();
            socket.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let config = root.path().join("config.toml");
        std::fs::write(&config,format!("default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://{address}/v1'\nresponse_max_retries=0\n[io]\nterm_grace_ms={term_grace_ms}\n")).unwrap();
        let owner = pty_owner(root.path(), &config, wrapper, controlling);
        let mut fixture = Self {
            root,
            owner,
            server,
        };
        timeout(Duration::from_secs(20), async {
            loop {
                fixture.owner.drain();
                // Locator publication precedes a usable request/reply round
                // trip. Probe with a read-only command, never retry creation.
                if fixture.root.path().join("data/endpoint.json").exists() {
                    match ipc::call(
                        &fixture.root.path().join("data"),
                        Command::ThreadList(ThreadListArgs::default()),
                        None,
                        None,
                    )
                    .await
                    {
                        Ok(response) => {
                            assert!(response.ok, "readiness: {:?}", response.error);
                            break;
                        }
                        Err(error) if error.code == ErrorCode::AppNotRunning => {}
                        Err(error) => panic!("owner readiness: {error:?}"),
                    }
                }
                assert!(
                    fixture.owner.child.try_wait().unwrap().is_none(),
                    "owner stopped during startup"
                );
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("owner startup watchdog");
        let directory = fixture.root.path().join("data");
        let thread = timeout(
            Duration::from_secs(15),
            ipc::call(
                &directory,
                Command::ThreadCreate(ThreadCreateArgs {
                    cwd: None,
                    parent_id: None,
                    connection: None,
                    model: None,
                    effort: None,
                }),
                None,
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap_or_else(|error| {
            fixture.owner.drain();
            panic!(
                "wrapper {wrapper:?} initial IPC: {error:?}\n{}",
                fixture.shutdown_diagnostic()
            )
        });
        assert!(thread.ok, "{:?}", thread.error);
        let thread = thread.result.unwrap()["thread_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let accepted = timeout(
            Duration::from_secs(15),
            ipc::call(
                &directory,
                Command::MessageSend(MessageSendArgs {
                    thread_id: Some(thread),
                    text: "wait for OS termination".into(),
                }),
                None,
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap_or_else(|error| {
            fixture.owner.drain();
            panic!(
                "wrapper {wrapper:?} message IPC: {error:?}\n{}",
                fixture.shutdown_diagnostic()
            )
        });
        assert!(accepted.ok, "{:?}", accepted.error);
        timeout(Duration::from_secs(20),async {loop {
            fixture.owner.drain();
            if let Ok(text)=std::fs::read_to_string(&pidfile) && text.ends_with('\n') && let Ok(pid)=text.trim().parse::<i32>() {
                assert!(pid>1);fixture.owner.native_group=Some(pid);
                let db=rusqlite::Connection::open_with_flags(directory.join("shellboy.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                let ready:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM shell_execution s JOIN output_chunk o ON o.execution_id=s.id WHERE s.state='running' AND o.bytes=CAST('SIGNAL_SHELL_READY' AS BLOB))",[],|r|r.get(0)).unwrap();if ready{break;}
            }
            assert!(fixture.owner.child.try_wait().unwrap().is_none());sleep(Duration::from_millis(5)).await;
        }}).await.expect("native shell did not reach running+saved output");
        if wrapper.is_some() {
            let owner_pid = std::fs::read_to_string(owner_pidfile)
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap();
            assert!(owner_pid > 1 && owner_pid != fixture.owner.child.id() as i32);
            fixture.owner.wrapped_owner = Some(owner_pid);
        }
        fixture
    }
    fn database(&self) -> PathBuf {
        self.root.path().join("data/shellboy.db")
    }
    fn log(&self) -> String {
        std::fs::read_to_string(self.root.path().join("data/logs/shellboy.log")).unwrap_or_default()
    }
    fn shutdown_diagnostic(&self) -> String {
        // Synthetic fixture data only. Keep failures useful after TempDir drops,
        // without printing output bodies or an unbounded diagnostic stream.
        let log = self.log();
        let mut start = log.len().saturating_sub(16 * 1024);
        while !log.is_char_boundary(start) {
            start += 1;
        }
        let mut states = Vec::new();
        match rusqlite::Connection::open_with_flags(
            self.database(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(db) => {
                for table in ["run", "tool_group", "tool_call", "shell_execution"] {
                    let result = (|| -> rusqlite::Result<Vec<(String, i64)>> {
                        let mut query = db.prepare(&format!(
                            "SELECT state,count(*) FROM {table} GROUP BY state LIMIT 16"
                        ))?;
                        query
                            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                            .collect()
                    })();
                    states.push(format!("{table}: {result:?}"));
                }
            }
            Err(error) => states.push(format!("database unavailable: {error}")),
        }
        format!(
            "{}\nlog tail:\n{}\nPTY tail:\n{}",
            states.join("\n"),
            &log[start..],
            String::from_utf8_lossy(&self.owner.terminal_tail)
        )
    }
    fn assert_graceful(&self) {
        let db = rusqlite::Connection::open_with_flags(
            self.database(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for table in ["tool_call", "tool_group"] {
            assert_eq!(
                db.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE state<>'completed'"),
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0,
                "unfinished {table}\n{}",
                self.shutdown_diagnostic()
            );
        }
        assert_eq!(
            db.query_row("SELECT count(*) FROM tool_result", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "{}",
            self.shutdown_diagnostic()
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM run WHERE state IN ('live','stopping')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "{}",
            self.shutdown_diagnostic()
        );
        assert_eq!(db.query_row("SELECT count(*) FROM shell_execution WHERE state IN ('starting','running','stopping')",[],|r|r.get::<_,i64>(0)).unwrap(),0,"{}",self.shutdown_diagnostic());
        assert_eq!(
            db.query_row("SELECT state,stop_reason FROM run", [], |r| Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?
            )))
            .unwrap(),
            ("stopped".into(), "app_shutdown".into()),
            "{}",
            self.shutdown_diagnostic()
        );
        assert_eq!(
            db.query_row("SELECT state FROM shell_execution", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "stopped",
            "{}",
            self.shutdown_diagnostic()
        );
        let records = self
            .log()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(records.iter().any(|v| v["shutdown"]["scope"] == "signal"));
        assert!(
            records
                .iter()
                .any(|v| v["shutdown"]["scope"] == "owner"
                    && v["shutdown"]["stage"] == "diagnostics")
        );
    }
    async fn wait_signal_log(&mut self) {
        timeout(Duration::from_secs(2), async {
            loop {
                self.owner.drain();
                if self.log().contains("signal_term") {
                    break;
                }
                assert!(self.owner.child.try_wait().unwrap().is_none());
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("first OS signal was not observed");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn term_and_hup_stop_native_shell_and_save_results_before_success_exit() {
    for (signal, stage) in [(libc::SIGTERM, "signal_term"), (libc::SIGHUP, "signal_hup")] {
        let mut fixture = Fixture::running().await;
        fixture.owner.signal(signal);
        let status = fixture.owner.exited(Duration::from_secs(8)).await;
        assert!(
            status.success(),
            "signal {signal}: {status}\n{}",
            fixture.shutdown_diagnostic()
        );
        fixture.assert_graceful();
        assert!(fixture.log().contains(stage));
        fixture.owner.native_group = None;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_real_pty_master_does_not_abandon_owner_cleanup() {
    let mut fixture = Fixture::running().await;
    let started = Instant::now();
    drop(fixture.owner.terminal.take());
    let status = fixture.owner.exited(Duration::from_secs(8)).await;
    assert!(
        status.success(),
        "PTY loss: {status}, elapsed {:?}\n{}",
        started.elapsed(),
        fixture.shutdown_diagnostic()
    );
    fixture.assert_graceful();
    fixture.owner.native_group = None;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_parent_shell_pty_close_kills_term_ignoring_owned_groups_without_force() {
    for wrapper in ["/bin/bash", "/bin/zsh"] {
        if !Path::new(wrapper).exists() {
            eprintln!("native parent shell {wrapper} unavailable; not verified");
            continue;
        }
        let mut fixture = Fixture::running_with(Some(wrapper), true, true, 2000).await;
        let owner = fixture.owner.wrapped_owner.unwrap();
        let native = fixture.owner.native_group.unwrap();
        let started = Instant::now();
        drop(fixture.owner.terminal.take());
        let stopped = timeout(Duration::from_secs(12), async {
            loop {
                let _ = fixture.owner.child.try_wait();
                let owner_gone = unsafe { libc::kill(owner, 0) } < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                let endpoint_gone = !fixture.root.path().join("data/endpoint.json").exists();
                if owner_gone && endpoint_gone {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            stopped.is_ok(),
            "{wrapper} owner failed to retire after {:?}\n{}",
            started.elapsed(),
            fixture.shutdown_diagnostic()
        );
        // The wrapper's wait status is not the child owner's status: HUP can
        // terminate it first. Durable cleanup and real process disappearance
        // establish successful owner shutdown independently.
        fixture.owner.wrapped_owner = None;
        eprintln!(
            "parent {wrapper} retired after {:?}; checking durable cleanup",
            started.elapsed()
        );
        fixture.assert_graceful();
        let log = fixture.log();
        assert!(
            !log.contains("signal_force") && !log.contains("signal_deadline"),
            "{wrapper}\n{log}"
        );
        assert!(
            unsafe { libc::kill(-native, 0) } < 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
            "{wrapper}: owned native process group remains"
        );
        fixture.owner.native_group = None;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eof_without_a_controlling_terminal_signal_still_drains_owner() {
    let mut fixture = Fixture::running_with(None, true, false, 2000).await;
    let native = fixture.owner.native_group.unwrap();
    drop(fixture.owner.terminal.take());
    let status = fixture.owner.exited(Duration::from_secs(12)).await;
    assert!(
        status.success(),
        "noncontrolling PTY EOF: {status}\n{}",
        fixture.shutdown_diagnostic()
    );
    fixture.assert_graceful();
    let log = fixture.log();
    assert!(
        log.contains("terminal_lost") && !log.contains("signal_hup"),
        "{log}"
    );
    assert!(
        unsafe { libc::kill(-native, 0) } < 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "owned native process group remains"
    );
    fixture.owner.native_group = None;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_signal_forces_without_waiting_for_blocked_database_cleanup() {
    let mut fixture = Fixture::running_with(None, true, true, 2000).await;
    let native = fixture.owner.native_group.unwrap();
    let hold = rusqlite::Connection::open(fixture.database()).unwrap();
    hold.execute_batch("BEGIN IMMEDIATE").unwrap();
    fixture.owner.signal(libc::SIGTERM);
    fixture.wait_signal_log().await;
    let start = Instant::now();
    fixture.owner.signal(libc::SIGTERM);
    let status = fixture.owner.exited(Duration::from_secs(2)).await;
    assert_eq!(status.code(), Some(1));
    assert!(start.elapsed() < Duration::from_secs(2));
    hold.execute_batch("ROLLBACK").unwrap();
    timeout(Duration::from_secs(2), async {
        while unsafe { libc::kill(-native, 0) } == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("forced exit left its known native process group alive");
    fixture.owner.native_group = None;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn term_followed_by_duplicate_hup_finishes_configured_native_grace() {
    let mut fixture = Fixture::running_with(None, true, true, 5500).await;
    fixture.owner.signal(libc::SIGTERM);
    fixture.wait_signal_log().await;
    for _ in 0..2 {
        fixture.owner.signal(libc::SIGHUP);
        sleep(Duration::from_millis(20)).await;
    }
    let status = fixture.owner.exited(Duration::from_secs(14)).await;
    assert!(
        status.success(),
        "TERM/HUP during configured grace: {status}\n{}",
        fixture.shutdown_diagnostic()
    );
    fixture.assert_graceful();
    let log = fixture.log();
    assert!(
        !log.contains("signal_force") && !log.contains("signal_deadline"),
        "{log}"
    );
    fixture.owner.native_group = None;
}
#[test]
#[ignore = "isolated production observer deadline subprocess"]
fn termination_budget_child() {
    let root = PathBuf::from(
        std::env::var_os("SHELLBOY_SIGNAL_BUDGET_ROOT").expect("isolated signal fixture"),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let termination = shellboy::ui::termination::Termination::install(runtime.handle()).unwrap();
    termination.bind_diagnostics(shellboy::diagnostics::Diagnostics::start(
        root.join("logs"),
        shellboy::config::Config::default().logging,
    ));
    std::fs::write(root.join("observer-ready"), b"ready").unwrap();
    // The production observer must enforce its bound independently of a UI
    // main thread that cannot make progress or complete cleanup.
    loop {
        std::thread::park();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_signal_deadline_is_enforced_even_when_the_main_thread_is_stalled() {
    let root = tempfile::tempdir().unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "termination_budget_child",
            "--ignored",
            "--nocapture",
        ])
        .env("SHELLBOY_SIGNAL_BUDGET_ROOT", root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut owner = Owner {
        child,
        terminal: None,
        native_group: None,
        wrapped_owner: None,
        terminal_tail: Vec::new(),
        finished: false,
    };
    timeout(Duration::from_secs(20), async {
        while !root.path().join("observer-ready").exists() {
            assert!(owner.child.try_wait().unwrap().is_none());
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("observer subprocess startup");
    let start = Instant::now();
    owner.signal(libc::SIGTERM);
    let status = owner.exited(Duration::from_secs(8)).await;
    assert_eq!(status.code(), Some(1));
    assert!(start.elapsed() >= Duration::from_secs(4));
    let log = std::fs::read_to_string(root.path().join("logs/shellboy.log")).unwrap();
    assert!(log.contains("signal_deadline"));
}
