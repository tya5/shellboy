#![cfg(windows)]
use shellboy::{
    config::{Config, ShellKind},
    protocol::{ShellState, ShellToolArgs},
    shell::*,
};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::timeout;
fn config() -> Config {
    let mut c = Config::default();
    c.shell.kind = ShellKind::Powershell;
    c.io.drain_timeout_ms = 2000;
    c
}
fn args(command: &str) -> ShellToolArgs {
    serde_json::from_value(serde_json::json!({"command":command})).unwrap()
}
async fn spawn(args: ShellToolArgs) -> ShellExecution {
    ShellSupervisor::new(&config())
        .spawn(
            "windows-test".into(),
            std::env::current_dir().unwrap(),
            args,
            BTreeMap::new(),
        )
        .await
        .unwrap()
}
async fn collect(mut execution: ShellExecution) -> (Vec<u8>, Vec<u8>, ShellCompletion) {
    timeout(Duration::from_secs(15), async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        while let Some(ShellEvent::Output(chunk)) = execution.events.recv().await {
            match chunk.stream {
                OutputStream::Stdout => out.extend(&chunk.bytes),
                OutputStream::Stderr => err.extend(&chunk.bytes),
            }
        }
        loop {
            if let Some(done) = execution.completion.borrow().clone() {
                return (out, err, done);
            }
            execution.completion.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn powershell_starts_with_hidden_environment_entries() {
    let environment = [
        ("=::", r"::\"),
        ("=ExitCode", "00000000"),
        ("=ExitCodeAscii", "00000000"),
        ("SHELLBOY_ENV_FIXTURE", "visible"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    let execution = ShellSupervisor::new(&config())
        .spawn(
            "windows-hidden-environment".into(),
            std::env::current_dir().unwrap(),
            args("[Console]::Out.Write($env:SHELLBOY_ENV_FIXTURE); exit 0"),
            environment,
        )
        .await
        .unwrap();
    let (out, err, done) = collect(execution).await;
    assert_eq!(out, b"visible");
    assert!(err.is_empty());
    assert_eq!(done.state, ShellState::Exited);
    assert_eq!(done.exit_code, Some(0));
    assert!(done.output_complete && done.group_cleanup_confirmed);
}

#[tokio::test]
async fn powershell_literal_command_quoting_stdout_stderr_and_exit_are_preserved() {
    let (out, err, done) = collect(
        spawn(args(
            "[Console]::Out.Write('a b\"c'); [Console]::Error.Write('error'); exit 7",
        ))
        .await,
    )
    .await;
    assert_eq!(out, b"a b\"c");
    assert_eq!(err, b"error");
    assert_eq!(done.state, ShellState::Exited);
    assert_eq!(done.exit_code, Some(7));
    assert!(done.output_complete && done.group_cleanup_confirmed);
}
#[tokio::test]
async fn stdin_and_eof_use_pipe_without_consuming_powershell_command_source() {
    let mut a = args(
        "[Console]::InputEncoding=[Text.UTF8Encoding]::new($false); [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); [Console]::Out.Write([Console]::In.ReadToEnd())",
    );
    a.stdin_open = true;
    a.initial_stdin = Some("initial:".into());
    let execution = spawn(a).await;
    assert_eq!(
        execution
            .handle
            .write("日本語", "UTF-8")
            .await
            .unwrap()
            .status,
        StdinWriteStatus::Complete
    );
    execution.handle.eof().await.unwrap();
    let (out, _, done) = collect(execution).await;
    assert_eq!(out, "initial:日本語".as_bytes());
    assert!(done.output_complete);
}
#[tokio::test]
async fn natural_shell_exit_and_stop_reclaim_job_descendants() {
    let command = "$p=[Diagnostics.ProcessStartInfo]::new(); $p.FileName='powershell.exe'; $p.Arguments='-NoLogo -NoProfile -NonInteractive -Command Start-Sleep 30'; $p.UseShellExecute=$false; $child=[Diagnostics.Process]::Start($p); [Console]::Out.Write($child.Id); exit 0";
    let (out, _, done) = collect(spawn(args(command)).await).await;
    let pid: String = String::from_utf8(out).unwrap();
    assert!(!pid.is_empty());
    assert!(done.forced && done.group_cleanup_confirmed && done.output_complete);
    let execution = spawn(args("Start-Sleep 30")).await;
    execution.handle.stop();
    execution.handle.stop();
    let (_, _, done) = collect(execution).await;
    assert_eq!(done.state, ShellState::Stopped);
    assert!(done.forced && done.group_cleanup_confirmed);
}

#[tokio::test]
async fn initial_input_larger_than_pipe_buffer_survives_default_eof() {
    let mut a = args(
        "[Console]::InputEncoding=[Text.UTF8Encoding]::new($false); $s=[Console]::In.ReadToEnd(); [Console]::Out.Write($s.Length)",
    );
    a.initial_stdin = Some("x".repeat(200000));
    let (out, _, done) = collect(spawn(a).await).await;
    assert_eq!(out, b"200000");
    assert!(done.output_complete);
}
#[tokio::test]
async fn blocked_native_stdin_timeout_counts_completed_bytes_and_stop_remains_independent() {
    let mut c = config();
    c.io.shell_write_timeout_ms = 60;
    let mut a = args("Start-Sleep 30");
    a.stdin_open = true;
    let execution = ShellSupervisor::new(&c)
        .spawn(
            "windows-blocked-stdin".into(),
            std::env::current_dir().unwrap(),
            a,
            BTreeMap::new(),
        )
        .await
        .unwrap();
    let result = execution
        .handle
        .write(&"x".repeat(1024 * 1024), "UTF-8")
        .await
        .unwrap();
    assert_eq!(result.status, StdinWriteStatus::Timeout);
    assert!(result.bytes_written < 1024 * 1024);
    assert!(!execution.handle.is_finished());
    execution.handle.stop();
    let (_, _, done) = collect(execution).await;
    assert_eq!(done.state, ShellState::Stopped);
    assert!(done.group_cleanup_confirmed);
}
