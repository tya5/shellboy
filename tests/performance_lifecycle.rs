#![cfg(unix)]
//! Opt-in real lifecycle measurements. No credentials, network service, or paid API.
use serde_json::{Value, json};
use shellboy::{
    app::DataOwner,
    attachments::{AttachmentInput, clipboard::ClipboardPixels},
    control::{Control, output::ShellOutputReader},
    protocol::*,
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct HttpCounts {
    inflight: AtomicUsize,
    peak: AtomicUsize,
    completed: AtomicUsize,
}
struct HttpGuard(Arc<HttpCounts>);
impl Drop for HttpGuard {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Proxy {
    url: String,
    plans: Arc<Mutex<HashMap<String, String>>>,
    counts: Arc<HttpCounts>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}
impl Proxy {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let plans = Arc::new(Mutex::new(HashMap::<String, String>::new()));
        let counts = Arc::new(HttpCounts::default());
        let stop = CancellationToken::new();
        let worker_plans = plans.clone();
        let worker_counts = counts.clone();
        let worker_stop = stop.clone();
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;_=worker_stop.cancelled()=>break,
                    accepted=listener.accept()=>{
                        let (mut stream,_)=accepted.unwrap();let plans=worker_plans.clone();let counts=worker_counts.clone();
                        handlers.spawn(async move {
                            let current=counts.inflight.fetch_add(1,Ordering::SeqCst)+1;counts.peak.fetch_max(current,Ordering::SeqCst);let _guard=HttpGuard(counts.clone());
                            let body=read_request(&mut stream).await;
                            let thread=body["input"].as_array().unwrap().iter().flat_map(|item|item["content"].as_array().into_iter().flatten()).find_map(|part|part["text"].as_str().and_then(|text|text.strip_prefix("Shellboy context v1: thread=")).map(|text|text.split_whitespace().next().unwrap().to_owned())).unwrap();
                            let command=plans.lock().unwrap().remove(&thread);
                            let output=if let Some(command)=command {json!([{"type":"function_call","id":uuid::Uuid::new_v4().to_string(),"call_id":uuid::Uuid::new_v4().to_string(),"name":"shell","arguments":json!({"command":command,"background":true}).to_string()}])}else{json!([{"type":"message","id":uuid::Uuid::new_v4().to_string(),"role":"assistant","status":"completed","content":[{"type":"output_text","text":"bounded lifecycle fixture answer"}]}])};
                            let reply=json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":output}).to_string();
                            tokio::time::sleep(Duration::from_millis(8)).await;
                            let head=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len());
                            stream.write_all(head.as_bytes()).await.unwrap();stream.write_all(reply.as_bytes()).await.unwrap();stream.shutdown().await.unwrap();counts.completed.fetch_add(1,Ordering::SeqCst);
                        });
                    },
                    result=handlers.join_next(),if !handlers.is_empty()=>{result.unwrap().unwrap();}
                }
            }
            while let Some(result) = handlers.join_next().await {
                result.unwrap();
            }
        });
        Self {
            url,
            plans,
            counts,
            stop,
            task,
        }
    }
    async fn shutdown(self) {
        self.stop.cancel();
        crate::test_watchdog::watchdog("tests/performance_lifecycle.rs: shutdown", self.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(self.counts.inflight.load(Ordering::SeqCst), 0);
    }
}
async fn read_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let split = loop {
        let mut chunk = [0; 4096];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(i) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let length = String::from_utf8_lossy(&bytes[..split])
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .and_then(|s| s.trim().parse::<usize>().ok())
        })
        .unwrap();
    while bytes.len() < split + length {
        let mut chunk = [0; 4096];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    serde_json::from_slice(&bytes[split..split + length]).unwrap()
}
fn operation(control: &Arc<Control>) -> String {
    format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4())
}
async fn command(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(operation(control), command))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}
fn settings() -> ThreadCreateArgs {
    ThreadCreateArgs {
        agents: None,
        connection: None,
        cwd: None,
        model: None,
        effort: None,
        parent_id: None,
    }
}
async fn thread(control: &Arc<Control>, parent: Option<String>) -> String {
    let mut args = settings();
    args.parent_id = parent;
    command(control, Command::ThreadCreate(args)).await["thread_id"]
        .as_str()
        .unwrap()
        .into()
}
async fn message(control: &Arc<Control>, thread: &str) -> MessageAccepted {
    serde_json::from_value(
        command(
            control,
            Command::MessageSend(MessageSendArgs {
                sender_label: None,
                now: false,
                thread_id: Some(thread.into()),
                text: "next lifecycle turn".into(),
            }),
        )
        .await,
    )
    .unwrap()
}
async fn until<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    crate::test_watchdog::watchdog("tests/performance_lifecycle.rs: until", async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn state(control: &Arc<Control>, table: &str, id: &str, wanted: &str) {
    until(|| {
        let db = control.store.database.clone();
        let table = table.to_owned();
        let id = id.to_owned();
        let wanted = wanted.to_owned();
        async move {
            db.call(move |db| {
                Ok(db.query_row(
                    &format!("SELECT state FROM {table} WHERE id=?1"),
                    [id],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap()
                == wanted
        }
    })
    .await;
}
async fn counts(control: &Arc<Control>) -> Value {
    control.store.database.call(|db|Ok(serde_json::from_str::<Value>(&db.query_row("SELECT json_object('live_runs',(SELECT count(*) FROM run WHERE state IN ('live','stopping')),'open_turns',(SELECT count(*) FROM turn WHERE state IN ('preparing','running','waiting','recovery_wait')),'live_shells',(SELECT count(*) FROM shell_execution WHERE state IN ('starting','running','stopping')),'runtime_pins',(SELECT count(*) FROM runtime_pin),'raw_pins',(SELECT count(*) FROM runtime_pin WHERE resource_kind='shell_output'),'pending_waits',(SELECT count(*) FROM app_meta WHERE key LIKE 'wait.pending.%'),'runs',(SELECT count(*) FROM run),'turns',(SELECT count(*) FROM turn),'shells',(SELECT count(*) FROM shell_execution),'nodes',(SELECT count(*) FROM node),'managed_files',(SELECT count(*) FROM managed_file WHERE state='ready'),'raw_bytes',(SELECT COALESCE(SUM(length(bytes)),0) FROM output_chunk))",[],|r|r.get::<_,String>(0))?).unwrap())).await.unwrap()
}
fn os_sample() -> Value {
    let pid = std::process::id();
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let rss = status
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmRSS:")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .map(|kb| kb * 1024);
        json!({"pid":pid,"rss_bytes":rss,"fds":std::fs::read_dir("/proc/self/fd").unwrap().count(),"threads":std::fs::read_dir("/proc/self/task").unwrap().count(),"method":"procfs; FD directory includes its sampling descriptor"})
    }
    #[cfg(not(target_os = "linux"))]
    {
        let run = |program: &str, args: &[&str]| {
            std::process::Command::new(program)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        };
        let id = pid.to_string();
        let rss = run("/bin/ps", &["-p", &id, "-o", "rss="])
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|kb| kb * 1024);
        let threads = run("/bin/ps", &["-M", "-p", &id, "-o", "pid="])
            .map(|s| s.lines().filter(|s| !s.trim().is_empty()).count());
        let fds = run("/usr/sbin/lsof", &["-a", "-p", &id, "-Ff"]).map(|s| {
            s.lines()
                .filter(|s| {
                    s.strip_prefix('f')
                        .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_digit()))
                })
                .count()
        });
        json!({"pid":pid,"rss_bytes":rss,"fds":fds,"threads":threads,"method":"ps rss/ps -M + lsof numeric FD; observer processes excluded"})
    }
}
async fn sample(
    control: &Arc<Control>,
    proxy: &Proxy,
    cycle: Option<usize>,
    phase: &str,
    elapsed: Duration,
) -> Value {
    let resources = counts(control).await;
    let storage = serde_json::to_value(control.gc.details().await.unwrap()).unwrap();
    let cache = control.attachments.cache_stats();
    let replay = control.replay_cache_stats();
    let os = tokio::task::spawn_blocking(os_sample).await.unwrap();
    let value = json!({"cycle":cycle,"phase":phase,"elapsed_ms":elapsed.as_secs_f64()*1000.0,"os":os,"resources":resources,"storage":storage,"attachment_cache":{"retained_bytes":cache.retained_bytes,"live_encoded_bytes":cache.live_encoded_bytes,"pinned_bytes":cache.pinned_bytes,"entries":cache.entries},"replay_cache":replay,"control_strong_count":Arc::strong_count(control),"proxy_inflight":proxy.counts.inflight.load(Ordering::SeqCst),"proxy_peak":proxy.counts.peak.load(Ordering::SeqCst),"proxy_completed":proxy.counts.completed.load(Ordering::SeqCst)});
    if phase.ends_with("released") || phase == "idle_12s" {
        assert_eq!(
            value["replay_cache"]["pinned_bytes"], 0,
            "HTTP replay lease survived {phase}"
        );
        assert!(
            value["replay_cache"]["retained_bytes"].as_u64().unwrap()
                <= control.store.config.cache.history_bytes,
            "retained replay cache exceeded history budget in {phase}"
        );
    }
    println!("LIFECYCLE {value}");
    value
}
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}
async fn pid(path: &Path) -> i32 {
    crate::test_watchdog::watchdog("tests/performance_lifecycle.rs: pid", async {
        loop {
            if let Ok(body) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = body.parse::<i32>()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

fn pixels(seed: u32) -> ClipboardPixels {
    let mut state = seed + 1;
    let rgba = (0..128 * 128 * 4)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        })
        .collect();
    ClipboardPixels {
        width: 128,
        height: 128,
        rgba,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real shell/HTTP lifecycle measurement; run --release --ignored --nocapture --test-threads=1"]
async fn repeated_owned_runs_release_resources_after_many_turns() {
    let cycles = std::env::var("SHELLBOY_PERF_LIFECYCLE_CYCLES")
        .ok()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(8);
    assert!((3..=32).contains(&cycles));
    let warmup = 2;
    let started = Instant::now();
    let proxy = Proxy::start().await;
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path,format!("default_connection='test'\ndefault_model='fixture'\n[connections.test]\nbase_url='{}'\n[cache]\nbody_bytes=131072\nlayout_bytes=131072\nhistory_bytes=131072\nattachment_base64_bytes=131072\n[io]\nterm_grace_ms=100\ndrain_timeout_ms=100\nself_reply_grace_ms=10\nshutdown_grace_ms=3000\n",proxy.url)).unwrap();
    let owner = DataOwner::start(
        Some(config_path),
        Some(directory.path().join("data")),
        directory.path().into(),
    )
    .await
    .unwrap();
    let control = Control::new(&owner, directory.path().into());
    control.start().await.unwrap();
    let (send, receive) = mpsc::channel(32);
    let local = shellboy::ipc::LocalServer::bind(&owner, send)
        .await
        .unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let parent = thread(&control, None).await;
    let child = thread(&control, Some(parent.clone())).await;
    println!(
        "LIFECYCLE_CONFIG {}",
        json!({"cycles":cycles,"warmup_cycles":warmup,"explicit_turns_per_thread_per_cycle":3,"tokio_worker_threads":4,"tokio_blocking_keep_alive":"Tokio default 10 seconds; final idle 12 seconds","cache":{"body":131072,"layout":131072,"history":131072,"attachment_base64":131072},"file_workers":{"attachment":owner.loaded.config.workers.attachment,"control_file":owner.loaded.config.workers.control_file},"ui_present":false,"index_embedding_configured":false,"managed_image":"per cycle 128x128 synthetic RGBA; retained by normal 7-day policy","raw_output":"two shells, 200 bounded lines each per cycle","OS":std::env::consts::OS,"architecture":std::env::consts::ARCH,"profile":if cfg!(debug_assertions){"debug"}else{"release"}})
    );
    sample(&control, &proxy, None, "baseline", started.elapsed()).await;
    let executable = env!("CARGO_BIN_EXE_shellboy").replace('\'', "'\\''");
    let mut released = Vec::new();
    for cycle in 0..cycles {
        let parent_pid = directory.path().join(format!("parent-{cycle}.pid"));
        let child_pid = directory.path().join(format!("child-{cycle}.pid"));
        let wait_pid = directory.path().join(format!("wait-{cycle}.pid"));
        let wait_gate = directory.path().join(format!("wait-{cycle}.gate"));
        let raw = "for ((i=0;i<200;i++)); do printf 'lifecycle output %s 日本語\\n' \"$i\"; done";
        let child_command = format!(
            "{raw}; printf '%s' $$ > '{}'; exec sleep 60",
            child_pid.display()
        );
        let parent_command = format!(
            r#"reply=$('{executable}' message send --thread '{child}' --text 'lifecycle child'); {raw}; printf '%s' $$ > '{parent_pid}'; while [[ ! -f '{wait_gate}' ]]; do sleep 0.02; done; pattern='"run_id":"([^"]+)"'; if [[ $reply =~ $pattern ]]; then run=${{BASH_REMATCH[1]}}; conditions=$(printf '[{{"type":"run_ended","run_id":"%s"}}]' "$run"); '{executable}' wait --conditions-json "$conditions" --timeout-ms 30000 & observer=$!; printf '%s' "$observer" > '{wait_pid}'; wait "$observer"; else exit 71; fi; exec sleep 60"#,
            parent_pid = parent_pid.display(),
            wait_pid = wait_pid.display(),
            wait_gate = wait_gate.display()
        );
        {
            let mut plans = proxy.plans.lock().unwrap();
            plans.insert(parent.clone(), parent_command);
            plans.insert(child.clone(), child_command);
        }
        let image = control
            .attachments
            .save_clipboard_pixels(pixels(cycle as u32), CancellationToken::new())
            .await
            .unwrap();
        let accepted: MessageAccepted = serde_json::from_value(
            control
                .send_draft(
                    operation(&control),
                    settings(),
                    Some(parent.clone()),
                    "lifecycle first turn".into(),
                    vec![AttachmentInput {
                        path: image.path.clone(),
                        kind: AttachmentKind::Image,
                    }],
                )
                .await
                .unwrap(),
        )
        .unwrap();
        drop(image);
        state(
            &control,
            "turn",
            accepted.turn_id.as_ref().unwrap(),
            "completed",
        )
        .await;
        let parent_process = pid(&parent_pid).await;
        let child_process = pid(&child_pid).await;
        let query_child = child.clone();
        let child_run: String = control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
                    [query_child],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        for _ in 1..3 {
            let next = message(&control, &parent).await;
            assert_eq!(next.run_id, accepted.run_id);
            state(
                &control,
                "turn",
                next.turn_id.as_ref().unwrap(),
                "completed",
            )
            .await;
            let next = message(&control, &child).await;
            assert_eq!(next.run_id.as_deref(), Some(child_run.as_str()));
            state(
                &control,
                "turn",
                next.turn_id.as_ref().unwrap(),
                "completed",
            )
            .await;
        }
        tokio::fs::write(&wait_gate, b"ready").await.unwrap();
        let pids = [parent_process, child_process, pid(&wait_pid).await];
        println!(
            "LIFECYCLE_PIDS {}",
            json!({"cycle":cycle,"owned_pids":pids})
        );
        assert!(pids.iter().all(|p| alive(*p)));
        let query_run = accepted.run_id.clone().unwrap();
        let execution: String = control
            .store
            .database
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT id FROM shell_execution WHERE owner_run_id=?1",
                    [query_run],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        let mut reader = ShellOutputReader::new(owner.db.clone(), owner.instance_id.clone())
            .stream(&execution)
            .await
            .unwrap();
        let page = reader.next_page(1024).await.unwrap().unwrap();
        assert!(page.raw_bytes > 0 && !page.done);
        let observer_control = control.clone();
        let target_run = accepted.run_id.clone().unwrap();
        let observer = tokio::spawn(async move {
            command(
                &observer_control,
                Command::Wait(WaitArgs {
                    after: None,
                    conditions: vec![WaitCondition::RunEnded { run_id: target_run }],
                    timeout_ms: 30000,
                }),
            )
            .await
        });
        until(|| async { counts(&control).await["pending_waits"] == 2 }).await;
        let active = sample(
            &control,
            &proxy,
            Some(cycle),
            if cycle < warmup {
                "warmup_active"
            } else {
                "steady_active"
            },
            started.elapsed(),
        )
        .await;
        assert_eq!(active["resources"]["live_runs"], 2);
        assert_eq!(active["resources"]["live_shells"], 2);
        assert_eq!(active["resources"]["raw_pins"], 1);
        let stopping = Instant::now();
        command(
            &control,
            Command::RunStop(RunTarget {
                run_id: accepted.run_id.clone().unwrap(),
            }),
        )
        .await;
        state(
            &control,
            "run",
            accepted.run_id.as_ref().unwrap(),
            "stopped",
        )
        .await;
        state(&control, "run", &child_run, "stopped").await;
        let stop_to_terminal = stopping.elapsed();
        assert_eq!(
            crate::test_watchdog::watchdog("tests/performance_lifecycle.rs: repeated_owned_runs_release_resources_after_many_turns", observer)
                .await
                .unwrap()
                .unwrap()["status"],
            "satisfied"
        );
        assert!(
            pids.iter().all(|p| !alive(*p)),
            "owned PID survived terminal state"
        );
        until(|| async {
            counts(&control).await["pending_waits"] == 0
                && proxy.counts.inflight.load(Ordering::SeqCst) == 0
        })
        .await;
        let ended = sample(
            &control,
            &proxy,
            Some(cycle),
            "ended_pin_held",
            started.elapsed(),
        )
        .await;
        assert_eq!(ended["resources"]["raw_pins"], 1);
        let releasing_pin = Instant::now();
        drop(reader);
        until(|| async { counts(&control).await["runtime_pins"] == 0 }).await;
        let pin_release = releasing_pin.elapsed();
        let selections = command(
            &control,
            Command::AttachmentList(ThreadTarget {
                thread_id: Some(parent.clone()),
            }),
        )
        .await;
        for item in selections.as_array().unwrap() {
            command(
                &control,
                Command::AttachmentRemove(AttachmentRemoveArgs {
                    thread_id: Some(parent.clone()),
                    attachment_id: item["attachment_id"].as_str().unwrap().into(),
                }),
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let value = sample(
            &control,
            &proxy,
            Some(cycle),
            if cycle < warmup {
                "warmup_released"
            } else {
                "steady_released"
            },
            started.elapsed(),
        )
        .await;
        for field in [
            "live_runs",
            "open_turns",
            "live_shells",
            "runtime_pins",
            "pending_waits",
        ] {
            assert_eq!(value["resources"][field], 0, "{field}");
        }
        assert_eq!(value["proxy_inflight"], 0);
        assert!(
            value["attachment_cache"]["retained_bytes"]
                .as_u64()
                .unwrap()
                <= 131072
        );
        assert_eq!(value["attachment_cache"]["pinned_bytes"], 0);
        println!(
            "LIFECYCLE_STOP {}",
            json!({"cycle":cycle,"stop_to_terminal_ms":stop_to_terminal.as_secs_f64()*1000.0,"pin_drop_to_zero_ms":pin_release.as_secs_f64()*1000.0,"stop_to_release_ms":stopping.elapsed().as_secs_f64()*1000.0})
        );
        released.push(value);
    }
    tokio::time::sleep(Duration::from_secs(12)).await;
    sample(&control, &proxy, None, "idle_12s", started.elapsed()).await;
    let shutdown = Instant::now();
    crate::test_watchdog::watchdog(
        "tests/performance_lifecycle.rs: repeated_owned_runs_release_resources_after_many_turns",
        control.shutdown(),
    )
    .await
    .unwrap()
    .unwrap();
    println!(
        "LIFECYCLE_SHUTDOWN {}",
        json!({"control_ms":shutdown.elapsed().as_secs_f64()*1000.0})
    );
    sample(
        &control,
        &proxy,
        None,
        "control_shutdown",
        started.elapsed(),
    )
    .await;
    local.shutdown().await.unwrap();
    serving.abort();
    let _ = serving.await;
    let weak = Arc::downgrade(&control);
    drop(control);
    until(|| async { weak.upgrade().is_none() }).await;
    let shutdown = Instant::now();
    owner.shutdown().await.unwrap();
    let owner_shutdown = shutdown.elapsed();
    let os = tokio::task::spawn_blocking(os_sample).await.unwrap();
    println!(
        "LIFECYCLE_OWNER_CLOSED {}",
        json!({"owner_ms":owner_shutdown.as_secs_f64()*1000.0,"os":os,"db_bytes":std::fs::metadata(directory.path().join("data/shellboy.db")).unwrap().len(),"wal_bytes":std::fs::metadata(directory.path().join("data/shellboy.db-wal")).map(|m|m.len()).unwrap_or(0)})
    );
    proxy.shutdown().await;
    println!(
        "LIFECYCLE_RESULT {}",
        json!({"cycles":cycles,"warmup":warmup,"steady_cycles":cycles-warmup,"released":released,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"assertions":"live DB states / wait metadata / runtime pins zero, owned PIDs gone, proxy inflight zero, bounded attachment/replay caches, replay HTTP pins zero at released/idle phases, Control weak dropped, shutdown completed; no absolute RSS assertion"})
    );
}
