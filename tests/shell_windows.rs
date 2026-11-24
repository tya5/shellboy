#![cfg(windows)]
use shellboy::{
    config::{Config, ShellKind},
    protocol::{ErrorCode, ShellState, ShellToolArgs},
    shell::*,
};
use std::collections::BTreeMap;
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
    crate::test_watchdog::watchdog("tests/shell_windows.rs: collect", async {
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
async fn invalid_native_launch_paths_identify_the_failed_field() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("not a directory.txt");
    std::fs::write(&file, b"not executable").unwrap();
    for (cwd, executable, field, code) in [
        (file.clone(), None, "cwd", ErrorCode::InvalidArgument),
        (
            root.path().to_owned(),
            Some(root.path().join("missing.exe")),
            "executable",
            ErrorCode::ConfigInvalid,
        ),
    ] {
        let mut cfg = config();
        cfg.shell.executable = executable;
        let error = match ShellSupervisor::new(&cfg)
            .spawn(
                "invalid-windows-path".into(),
                cwd,
                args("exit 0"),
                BTreeMap::new(),
            )
            .await
        {
            Ok(_) => panic!("invalid launch path started a shell"),
            Err(error) => error,
        };
        assert_eq!(error.code, code);
        let details = error.details.unwrap();
        assert_eq!(details["reason"], "windows_shell_launch_path");
        assert_eq!(details["field"], field);
    }
}

#[tokio::test]
async fn restricted_parent_job_reports_shell_launch_denial() {
    use std::{
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr::null,
    };
    use windows_sys::Win32::System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOBOBJECT_BASIC_LIMIT_INFORMATION, JobObjectBasicLimitInformation,
            SetInformationJobObject,
        },
        Threading::GetCurrentProcess,
    };

    // Restrict only a disposable test process, never the shared test runner.
    const CHILD: &str = "SHELLBOY_TEST_RESTRICTED_PARENT_JOB";
    if std::env::var_os(CHILD).is_none() {
        let child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "shell_windows::restricted_parent_job_reports_shell_launch_denial",
            ])
            .env(CHILD, "1")
            .kill_on_drop(true)
            .output();
        let output = crate::test_watchdog::watchdog("restricted Windows parent Job", child)
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    let job = unsafe { CreateJobObjectW(null(), null()) };
    assert!(!job.is_null(), "{}", std::io::Error::last_os_error());
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let limits = JOBOBJECT_BASIC_LIMIT_INFORMATION {
        LimitFlags: JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        ActiveProcessLimit: 1,
        ..Default::default()
    };
    assert_ne!(
        unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>() as u32,
            )
        },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
    assert_ne!(
        unsafe { AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
    let error = match ShellSupervisor::new(&config())
        .spawn(
            "restricted-job-test".into(),
            std::env::current_dir().unwrap(),
            args("exit 0"),
            BTreeMap::new(),
        )
        .await
    {
        Ok(_) => panic!("restricted parent Job unexpectedly allowed shell launch"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_eq!(
        error.details.unwrap()["reason"],
        "windows_parent_job_launch_denied"
    );
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
        "[Console]::Out.Write([Console]::In.ReadToEnd()); [Console]::Error.WriteLine('標準エラー')",
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
    assert_eq!(
        execution.handle.eof().await.unwrap_err().code,
        ErrorCode::InvalidState
    );
    assert_eq!(
        execution
            .handle
            .write("late", "UTF-8")
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidState
    );
    let (out, err, done) = collect(execution).await;
    assert_eq!(out, "initial:日本語".as_bytes());
    assert_eq!(err, "標準エラー\r\n".as_bytes());
    assert!(done.output_complete);
}
#[tokio::test]
async fn default_stdin_delivers_initial_input_then_closes_and_rejects_late_write() {
    let mut input = args("[Console]::Out.Write([Console]::In.ReadToEnd())");
    input.initial_stdin = Some("initial".into());
    let execution = spawn(input).await;
    let write = execution.handle.write("late", "UTF-8").await;
    let eof = execution.handle.eof().await;
    let (out, _, done) = collect(execution).await;
    assert_eq!(out, b"initial");
    assert!(done.output_complete);
    assert_eq!(write.unwrap_err().code, ErrorCode::InvalidState);
    assert_eq!(eof.unwrap_err().code, ErrorCode::InvalidState);
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
