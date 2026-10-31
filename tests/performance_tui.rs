#![cfg(target_os = "macos")]
//! Opt-in real-binary PTY measurements. No model request or submitted message.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};
const SAMPLES: usize = 7;
const IDLE: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

fn command(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "metadata command failed: {program}"
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn binary_hash(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let read = file.read(&mut bytes).unwrap();
        if read == 0 {
            break;
        }
        digest.update(&bytes[..read]);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn host_load() -> [f64; 3] {
    let mut load = [0.0; 3];
    // SAFETY: the buffer has the declared three writable doubles.
    assert_eq!(unsafe { libc::getloadavg(load.as_mut_ptr(), 3) }, 3);
    load
}
fn report(metric: &str, unit: &str, values: &[f64], scope: &str) {
    assert!(values.len() >= SAMPLES);
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        sorted[((p * sorted.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1)]
    };
    println!(
        "PERF_TUI {}",
        json!({"metric":metric,"unit":unit,"n":values.len(),"p50":percentile(0.5),"p95":percentile(0.95),"max":sorted.last(),"samples":values,"scope":scope})
    );
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
        assert!(self.bytes <= MAX_OUTPUT, "terminal output flood");
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
}
impl Drop for Tui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
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
        let deadline = crate::test_watchdog::Deadline::new("native PTY/process fixture");
        loop {
            self.drain();
            if self.output.contains(needle) {
                return;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "TUI exited before measured output"
            );
            assert!(!deadline.expired(), "TUI output deadline exceeded");
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
    fn paste(&mut self, marker: &str) -> Duration {
        self.drain();
        self.output.clear();
        let payload = format!("\x1b[200~{marker}\x1b[201~");
        let start = Instant::now();
        self.terminal.write_all(payload.as_bytes()).unwrap();
        self.until(marker.as_bytes());
        start.elapsed()
    }
    fn idle(&mut self) -> Idle {
        // Allow first-frame work and delayed startup observations to settle.
        std::thread::sleep(Duration::from_millis(500));
        self.drain();
        self.output.clear();
        let first = ps(self.child.id());
        let start = Instant::now();
        let mut rss_max = first.rss_kib;
        let mut last = first;
        for step in 1..=4 {
            let target = start + IDLE.mul_f64(step as f64 / 4.0);
            std::thread::sleep(target.saturating_duration_since(Instant::now()));
            self.drain();
            last = ps(self.child.id());
            rss_max = rss_max.max(last.rss_kib);
        }
        let wall = start.elapsed().as_secs_f64();
        Idle {
            rss_max_kib: rss_max,
            cpu_percent: (last.cpu_seconds - first.cpu_seconds).max(0.0) / wall * 100.0,
            ps_cpu_end_percent: last.ps_cpu_percent,
            observed_seconds: wall,
            terminal_bytes: self.output.bytes,
        }
    }
    fn quit(&mut self) {
        self.terminal.write_all(&[0x11]).unwrap();
        let deadline = crate::test_watchdog::Deadline::new("native PTY/process fixture");
        loop {
            self.drain();
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(
                !deadline.expired(),
                "graceful TUI shutdown deadline exceeded"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
#[derive(Clone, Copy)]
struct Ps {
    rss_kib: f64,
    cpu_seconds: f64,
    ps_cpu_percent: f64,
}
struct Idle {
    rss_max_kib: f64,
    cpu_percent: f64,
    ps_cpu_end_percent: f64,
    observed_seconds: f64,
    terminal_bytes: usize,
}
fn ps(pid: u32) -> Ps {
    let text = command(
        "/bin/ps",
        &[
            "-p",
            &pid.to_string(),
            "-o",
            "rss=",
            "-o",
            "time=",
            "-o",
            "%cpu=",
        ],
    );
    let fields = text.split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 3, "unexpected macOS ps format");
    let (days, clock) = fields[1]
        .split_once('-')
        .map_or((0.0, fields[1]), |(days, clock)| {
            (days.parse::<f64>().unwrap(), clock)
        });
    let clock = clock
        .split(':')
        .map(|part| part.parse::<f64>().unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(clock.len(), 2 | 3));
    let cpu_seconds = days * 86400.0 + clock.iter().fold(0.0, |total, part| total * 60.0 + part);
    Ps {
        rss_kib: fields[0].parse().unwrap(),
        cpu_seconds,
        ps_cpu_percent: fields[2].parse().unwrap(),
    }
}
#[test]
#[ignore = "real macOS TUI x14; ~40 seconds; explicitly run after source freeze"]
fn performance_real_tui_startup_paste_and_idle() {
    let binary = std::env::var_os("SHELLBOY_PERF_TUI_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_shellboy").into());
    let metadata: Value = json!({"schema":1,"os":command("/usr/bin/uname", &["-srm"]),"macos":command("/usr/bin/sw_vers", &["-productVersion"]),"os_build":command("/usr/bin/sw_vers", &["-buildVersion"]),"hardware":command("/usr/sbin/sysctl", &["-n","hw.model","hw.ncpu","hw.memsize"]),"rustc":command("rustc", &["--version","--verbose"]),"binary_sha256":binary_hash(&binary),"test_debug_assertions":cfg!(debug_assertions),"binary_override":std::env::var_os("SHELLBOY_PERF_TUI_BIN").is_some(),"git_commit":command("git", &["rev-parse","HEAD"]),"load_average_start":host_load(),"samples_per_mode":SAMPLES,"terminal":{"term":"xterm-256color","columns":120,"rows":32},"settings":{"config":"empty TOML; Config defaults; no connections or embedding; no Send","ui_max_fps":shellboy::config::Config::default().ui.max_fps},"idle_requested_seconds":IDLE.as_secs_f64(),"ps_rss_unit":"KiB (1024 bytes)","ps_cpu_time_display_resolution_seconds":0.01,"ps_percent_cpu":"macOS decaying average; not the 2-second interval percentage","latency_boundary":"before PTY write of complete Japanese bracketed-paste frame to receipt of full marker in ANSI-stripped PTY output; not pixels, hardware keypress, IME or terminal-emulator processing","startup_boundary":"before spawning real shellboy to first input-ready footer with raw ECHO disabled; PTY/config/tempdir creation excluded","fresh_semantics":"new data directory and migrations; OS/binary caches are not flushed; binary hash read warms executable pages"});
    println!("PERF_TUI_ENV {metadata}");
    let mut fresh = Vec::new();
    let mut warm = Vec::new();
    let mut fresh_paste = Vec::new();
    let mut warm_paste = Vec::new();
    let mut fresh_idle = Vec::new();
    let mut warm_idle = Vec::new();
    for sample in 0..SAMPLES {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let data = root.path().join("data");
        for reopen in [false, true] {
            let load = host_load();
            let (mut tui, started) = Tui::start(&binary, &config, &data, root.path());
            let marker = format!(
                "日本語貼付試験{}{:02}",
                if reopen { 'W' } else { 'F' },
                sample
            );
            let paste = tui.paste(&marker);
            let idle = tui.idle();
            println!(
                "PERF_TUI_SAMPLE {}",
                json!({"mode":if reopen{"warm_reopen"}else{"fresh_data"},"sample":sample,"load_before":load,"startup_us":started.as_secs_f64()*1e6,"paste_to_output_us":paste.as_secs_f64()*1e6,"idle_wall_seconds":idle.observed_seconds,"idle_terminal_bytes":idle.terminal_bytes,"idle_rss_max_kib":idle.rss_max_kib,"idle_cpu_interval_pct":idle.cpu_percent,"idle_ps_cpu_end_pct":idle.ps_cpu_end_percent})
            );
            tui.quit();
            if reopen {
                warm.push(started.as_secs_f64() * 1e6);
                warm_paste.push(paste.as_secs_f64() * 1e6);
                warm_idle.push(idle);
            } else {
                fresh.push(started.as_secs_f64() * 1e6);
                fresh_paste.push(paste.as_secs_f64() * 1e6);
                fresh_idle.push(idle);
            }
        }
    }
    report(
        "startup_fresh_data",
        "us",
        &fresh,
        "real binary; new isolated data-dir; executable/OS caches not flushed",
    );
    report(
        "startup_warm_reopen",
        "us",
        &warm,
        "same data-dir after graceful close; no submitted conversation/history",
    );
    report(
        "paste_fresh",
        "us",
        &fresh_paste,
        "first Japanese bracketed paste after first rendered footer",
    );
    report(
        "paste_warm",
        "us",
        &warm_paste,
        "first Japanese bracketed paste after warm reopen",
    );
    for (mode, samples) in [("fresh", fresh_idle), ("warm", warm_idle)] {
        report(
            &format!("idle_rss_max_{mode}"),
            "KiB",
            &samples.iter().map(|s| s.rss_max_kib).collect::<Vec<_>>(),
            "max of five ps resident snapshots over approximately two seconds",
        );
        report(
            &format!("idle_cpu_interval_{mode}"),
            "percent",
            &samples.iter().map(|s| s.cpu_percent).collect::<Vec<_>>(),
            "ps accumulated user+system CPU delta / measured wall; nominal CPU counter quantization 0.01s",
        );
        report(
            &format!("idle_ps_cpu_end_{mode}"),
            "percent",
            &samples
                .iter()
                .map(|s| s.ps_cpu_end_percent)
                .collect::<Vec<_>>(),
            "macOS ps decaying average snapshot; contains startup history",
        );
    }
    println!(
        "PERF_TUI_END {}",
        json!({"load_average_end":host_load(),"acceptance":"measurements only; no absolute performance SLO asserted"})
    );
}
