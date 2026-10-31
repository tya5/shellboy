#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use shellboy::{
    config::{Config, ShellKind},
    protocol::{ShellState, ShellToolArgs},
};
use shellboy::{protocol::ErrorCode, shell::*};
#[cfg(unix)]
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

#[cfg(unix)]
fn config() -> Config {
    let mut config = Config::default();
    config.shell.kind = ShellKind::Bash;
    config.io.term_grace_ms = 120;
    config.io.drain_timeout_ms = 300;
    config.io.shell_write_timeout_ms = 200;
    config
}
#[cfg(unix)]
fn args(command: &str) -> ShellToolArgs {
    serde_json::from_value(json!({"command":command})).unwrap()
}
#[cfg(unix)]
async fn spawn(config: &Config, args: ShellToolArgs) -> ShellExecution {
    ShellSupervisor::new(config)
        .spawn(
            "test-execution".into(),
            PathBuf::from("/"),
            args,
            BTreeMap::new(),
        )
        .await
        .unwrap()
}
#[cfg(unix)]
async fn collect(
    mut execution: ShellExecution,
) -> (Vec<(u64, OutputStream, Vec<u8>)>, ShellCompletion) {
    let mut chunks = Vec::new();
    let result = crate::test_watchdog::watchdog("tests/shell.rs: collect", async {
        while let Some(ShellEvent::Output(chunk)) = execution.events.recv().await {
            chunks.push((chunk.sequence, chunk.stream, chunk.bytes.clone()));
        }
        loop {
            if let Some(done) = execution.completion.borrow().clone() {
                return (std::mem::take(&mut chunks), done);
            }
            execution.completion.changed().await.unwrap();
        }
    })
    .await;
    result.unwrap_or_else(|error|panic!("shell must complete within finite deadline: {error}; completion={:?}; handle_finished={}; raw_chunks={}",execution.completion.borrow().clone(),execution.handle.is_finished(),chunks.len()))
}
#[cfg(unix)]
fn process_diagnostic(pid: i32) -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .unwrap_or_else(|error| format!("process stat unavailable: {error}"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        format!("process {pid} remains visible to kill(pid, 0)")
    }
}

#[cfg(unix)]
fn joined(chunks: &[(u64, OutputStream, Vec<u8>)], stream: OutputStream) -> Vec<u8> {
    chunks
        .iter()
        .filter(|(_, s, _)| *s == stream)
        .flat_map(|(_, _, b)| b.iter().copied())
        .collect()
}

#[test]
fn input_encoding_is_strict_complete_and_byte_limited() {
    assert_eq!(
        encode_input("日本語", "cp932", 10).unwrap(),
        [0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]
    );
    assert_eq!(
        encode_input("A\u{1f600}", "UTF-16LE", 6).unwrap(),
        [65, 0, 0x3d, 0xd8, 0, 0xde]
    );
    assert_eq!(encode_input("A", "UTF-16BE", 2).unwrap(), [0, 65]);
    assert_eq!(encode_input("A", "unicodefffe", 2).unwrap(), [0, 65]);
    assert_eq!(
        encode_input("\u{1f600}", "cp932", 100).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        encode_input("日", "UTF-8", 2).unwrap_err().code,
        ErrorCode::InputLimit
    );
    assert!(encode_input("A", "UTF-16", 10).is_err());
    assert!(encode_input("A", "invented-encoding", 10).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn bash_preserves_command_exit_status_multistream_and_raw_non_utf8() {
    let execution = spawn(
        &config(),
        args("printf 'stdout-no-newline'; printf '\\377\\376' >&2; exit 7"),
    )
    .await;
    let (chunks, done) = collect(execution).await;
    assert_eq!(joined(&chunks, OutputStream::Stdout), b"stdout-no-newline");
    assert_eq!(joined(&chunks, OutputStream::Stderr), [255, 254]);
    for (i, (sequence, _, _)) in chunks.iter().enumerate() {
        assert_eq!(*sequence, i as u64);
    }
    assert_eq!(done.state, ShellState::Exited);
    assert_eq!(done.exit_code, Some(7));
    assert!(done.output_complete && done.group_cleanup_confirmed);
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_default_eof_initial_input_and_explicit_write_eof_are_ordered() {
    let (chunks, done) = collect(spawn(&config(), args("cat; printf ':eof'")).await).await;
    assert_eq!(joined(&chunks, OutputStream::Stdout), b":eof");
    assert!(done.output_complete);
    let mut input = args("cat");
    input.initial_stdin = Some("first:".into());
    input.stdin_open = true;
    let execution = spawn(&config(), input).await;
    let result = execution.handle.write("日本語", "cp932").await.unwrap();
    assert_eq!(result.bytes_written, 6);
    assert_eq!(result.status, StdinWriteStatus::Complete);
    execution.handle.eof().await.unwrap();
    execution.handle.eof().await.unwrap();
    assert_eq!(
        execution
            .handle
            .write("late", "UTF-8")
            .await
            .unwrap()
            .status,
        StdinWriteStatus::Closed
    );
    let (chunks, done) = collect(execution).await;
    assert_eq!(
        joined(&chunks, OutputStream::Stdout),
        [b"first:".as_slice(), &[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]].concat()
    );
    assert_eq!(done.state, ShellState::Exited);
    let mut input = args("cat");
    input.initial_stdin = Some("closed-after-initial".into());
    let (chunks, _) = collect(spawn(&config(), input).await).await;
    assert_eq!(
        joined(&chunks, OutputStream::Stdout),
        b"closed-after-initial"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn encoding_failure_happens_before_os_spawn_and_before_any_partial_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-exist");
    let mut input = args(&format!("touch '{}'", path.display()));
    input.initial_stdin = Some("\u{1f600}".into());
    input.input_encoding = "CP932".into();
    let error = ShellSupervisor::new(&config())
        .spawn("id".into(), dir.path().into(), input, BTreeMap::new())
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(!path.exists());
    let mut input = args("cat");
    input.stdin_open = true;
    let execution = spawn(&config(), input).await;
    assert!(
        execution
            .handle
            .write("prefix\u{1f600}", "CP932")
            .await
            .is_err()
    );
    execution.handle.eof().await.unwrap();
    let (chunks, _) = collect(execution).await;
    assert!(joined(&chunks, OutputStream::Stdout).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn stop_is_idempotent_and_independent_of_a_blocked_stdin_writer() {
    let mut cfg = config();
    cfg.io.shell_write_timeout_ms = 10_000;
    let mut input = args("trap '' TERM; printf ready; sleep 30");
    input.stdin_open = true;
    let mut execution = spawn(&cfg, input).await;
    let ShellEvent::Output(ready) = crate::test_watchdog::watchdog(
        "tests/shell.rs: stop_is_idempotent_and_independent_of_a_blocked_stdin_writer",
        execution.events.recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ready.bytes, b"ready");
    drop(ready);
    let writer = execution.handle.clone();
    let write_task = tokio::spawn(async move {
        writer
            .write(&"x".repeat(1024 * 1024), "UTF-8")
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    execution.handle.stop();
    execution.handle.stop();
    let write = crate::test_watchdog::watchdog(
        "tests/shell.rs: stop_is_idempotent_and_independent_of_a_blocked_stdin_writer",
        write_task,
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(write.status, StdinWriteStatus::Complete);
    let (_, done) = collect(execution).await;
    assert_eq!(done.state, ShellState::Stopped);
    assert!(done.forced);
}

#[cfg(unix)]
#[tokio::test]
async fn write_deadline_reports_partial_bytes_without_retrying_or_stopping_target() {
    let mut cfg = config();
    cfg.io.shell_write_timeout_ms = 50;
    let mut input = args("sleep 30");
    input.stdin_open = true;
    let execution = spawn(&cfg, input).await;
    let written = execution
        .handle
        .write(&"x".repeat(1024 * 1024), "UTF-8")
        .await
        .unwrap();
    assert_eq!(written.status, StdinWriteStatus::Timeout);
    assert!(written.bytes_written < 1024 * 1024);
    assert!(!execution.handle.is_finished());
    execution.handle.stop();
    let (_, done) = collect(execution).await;
    assert_eq!(done.state, ShellState::Stopped);
}

#[cfg(unix)]
#[tokio::test]
async fn shell_natural_exit_cleans_descendants_that_keep_pipes_open() {
    let execution = spawn(&config(), args("sleep 30 & printf '%s' \"$!\"; exit 0")).await;
    let (chunks, done) = collect(execution).await;
    let pid: i32 = String::from_utf8(joined(&chunks, OutputStream::Stdout))
        .unwrap()
        .parse()
        .unwrap();
    let live = unsafe { libc::kill(pid, 0) } == 0;
    assert!(
        !live,
        "owned descendant {pid} remains: {}",
        process_diagnostic(pid)
    );
    assert_eq!(done.exit_code, Some(0));
    assert!(done.output_complete && done.group_cleanup_confirmed);
}

#[cfg(unix)]
#[tokio::test]
async fn raw_limit_stops_infinite_producer_with_explicit_truncation() {
    let mut cfg = config();
    cfg.limits.shell_raw_bytes = 1000;
    let execution = spawn(
        &cfg,
        args("while :; do printf 'abcdefghijklmnopqrstuvwxyz'; done"),
    )
    .await;
    let (chunks, done) = collect(execution).await;
    assert_eq!(chunks.iter().map(|(_, _, b)| b.len()).sum::<usize>(), 1000);
    assert_eq!(done.state, ShellState::Failed);
    assert!(!done.output_complete);
    assert!(done.reason.unwrap().contains("raw output limit"));
}

#[cfg(unix)]
#[tokio::test]
async fn byte_backpressure_does_not_block_stop_or_starve_stderr_only_output() {
    let mut cfg = config();
    cfg.io.output_buffer_bytes = 16;
    cfg.io.output_batch_bytes = 16;
    let (chunks, done) = collect(spawn(&cfg, args("printf 'stderr-only' >&2")).await).await;
    assert_eq!(joined(&chunks, OutputStream::Stderr), b"stderr-only");
    assert!(done.output_complete);
    let execution = spawn(
        &cfg,
        args("while :; do printf 'abcdefghijklmnopqrstuvwxyz'; done"),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    execution.handle.stop();
    let mut completion = execution.completion.clone();
    crate::test_watchdog::watchdog(
        "tests/shell.rs: byte_backpressure_does_not_block_stop_or_starve_stderr_only_output",
        async {
            loop {
                if completion.borrow().is_some() {
                    break;
                }
                completion.changed().await.unwrap();
            }
        },
    )
    .await
    .unwrap();
    let (_, done) = collect(execution).await;
    assert!(!done.output_complete);
}

#[cfg(unix)]
#[tokio::test]
async fn environment_overrides_are_injected_without_removing_bash_env() {
    let mut cfg = config();
    cfg.shell
        .env
        .insert("SHELLBOY_TEST".into(), "configured".into());
    let dir = tempfile::tempdir().unwrap();
    let bash_env = dir.path().join("bash-env");
    std::fs::write(&bash_env, "export FROM_BASH_ENV=loaded\n").unwrap();
    let mut env = BTreeMap::new();
    env.insert("SHELLBOY_TEST".into(), "origin".into());
    env.insert("BASH_ENV".into(), bash_env.to_string_lossy().into_owned());
    let execution = ShellSupervisor::new(&cfg)
        .spawn(
            "env".into(),
            PathBuf::from("/"),
            args("printf '%s:%s' \"$SHELLBOY_TEST\" \"$FROM_BASH_ENV\""),
            env,
        )
        .await
        .unwrap();
    let (chunks, _) = collect(execution).await;
    assert_eq!(joined(&chunks, OutputStream::Stdout), b"origin:loaded");
}

#[cfg(unix)]
#[tokio::test]
async fn last_handle_drop_requests_cleanup_and_natural_exit_kills_term_ignoring_child() {
    let execution = spawn(&config(), args("sleep 30")).await;
    let ShellExecution {
        handle,
        mut events,
        mut completion,
    } = execution;
    drop(handle);
    crate::test_watchdog::watchdog("tests/shell.rs: last_handle_drop_requests_cleanup_and_natural_exit_kills_term_ignoring_child", async {
        while events.recv().await.is_some() {}
        loop {
            if let Some(done) = completion.borrow().clone() {
                assert_eq!(done.state, ShellState::Stopped);
                break;
            }
            completion.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let execution = spawn(
        &config(),
        args("trap '' TERM; sleep 30 & printf '%s' \"$!\"; exit 0"),
    )
    .await;
    let (chunks, done) = collect(execution).await;
    let pid: i32 = String::from_utf8(joined(&chunks, OutputStream::Stdout))
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "owned descendant {pid} remains: {}",
        process_diagnostic(pid)
    );
    assert_eq!(done.exit_code, Some(0));
    assert!(done.forced && done.group_cleanup_confirmed && done.output_complete);
}

#[cfg(unix)]
#[tokio::test]
async fn leading_dash_command_is_a_script_not_a_bash_option() {
    let root = tempfile::tempdir().unwrap();
    // Use an already installed native executable: this regression concerns
    // Bash argv, not first-launch behavior of a newly written interpreter file.
    std::os::unix::fs::symlink("/bin/echo", root.path().join("-x")).unwrap();
    let mut config = config();
    config.shell.env.insert(
        "PATH".into(),
        format!(
            "{}:{}",
            root.path().display(),
            std::env::var("PATH").unwrap()
        ),
    );
    let (chunks, completion) = collect(spawn(&config, args("-x 'arg space'")).await).await;
    assert_eq!(joined(&chunks, OutputStream::Stdout), b"arg space\n");
    assert_eq!(completion.exit_code, Some(0));
    assert_eq!(completion.state, ShellState::Exited);
    assert!(completion.output_complete);
}

#[cfg(unix)]
#[tokio::test]
async fn successful_exit_keeps_its_state_when_output_drain_is_incomplete() {
    let mut cfg = config();
    cfg.io.output_buffer_bytes = 16;
    cfg.io.output_batch_bytes = 16;
    cfg.io.drain_timeout_ms = 50;
    let mut execution = spawn(
        &cfg,
        args("printf 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
    )
    .await;
    // Hold the bounded output queue unconsumed until the real supervisor's
    // finite drain ends. The child can write 64 bytes to the OS pipe and exit.
    crate::test_watchdog::watchdog(
        "tests/shell.rs: successful_exit_keeps_its_state_when_output_drain_is_incomplete",
        async {
            loop {
                if execution.completion.borrow().is_some() {
                    break;
                }
                execution.completion.changed().await.unwrap();
            }
        },
    )
    .await
    .unwrap();
    let (_, done) = collect(execution).await;
    assert_eq!(done.exit_code, Some(0));
    assert!(!done.output_complete);
    assert!(done.reason.as_deref().unwrap().contains("drain"));
    assert_eq!(
        done.state,
        ShellState::Exited,
        "output observation does not change the successful process result"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stop_after_reaped_natural_exit_does_not_relabel_the_process_as_stopped() {
    let mut cfg = config();
    cfg.io.output_buffer_bytes = 16;
    cfg.io.output_batch_bytes = 16;
    cfg.io.drain_timeout_ms = 1000;
    let mut execution=spawn(&cfg,args("printf '%s\\n' \"$$\"; printf 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")).await;
    let mut held = Vec::new();
    let mut prefix = Vec::new();
    let pid = crate::test_watchdog::watchdog(
        "tests/shell.rs: stop_after_reaped_natural_exit_does_not_relabel_the_process_as_stopped",
        async {
            loop {
                let Some(ShellEvent::Output(chunk)) = execution.events.recv().await else {
                    panic!("fixture must emit PID");
                };
                prefix.extend_from_slice(&chunk.bytes);
                held.push(chunk);
                if let Some(at) = prefix.iter().position(|&b| b == b'\n') {
                    break std::str::from_utf8(&prefix[..at])
                        .unwrap()
                        .parse::<i32>()
                        .unwrap();
                }
            }
        },
    )
    .await
    .unwrap();
    crate::test_watchdog::watchdog(
        "tests/shell.rs: stop_after_reaped_natural_exit_does_not_relabel_the_process_as_stopped",
        async {
            loop {
                // Signal zero observes the actual direct child's reap, not a sleep
                // estimate. Retained byte permits keep the later drain pending.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::ESRCH)
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        },
    )
    .await
    .unwrap();
    assert!(
        execution.completion.borrow().is_none(),
        "fixture must still be in output drain"
    );
    execution.handle.stop();
    drop(held);
    let (chunks, done) = collect(execution).await;
    prefix.extend(joined(&chunks, OutputStream::Stdout));
    assert!(prefix.ends_with(b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"));
    assert_eq!(done.exit_code, Some(0));
    assert!(done.output_complete);
    assert_eq!(
        done.state,
        ShellState::Exited,
        "a later stop cannot change the already reaped process outcome"
    );
}
