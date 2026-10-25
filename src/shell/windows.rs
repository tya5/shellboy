//! Native Windows process creation with an atomic Job assignment. There is no
//! CREATE_SUSPENDED/AssignProcessToJobObject window and no inheritable Job handle.
use super::windows_command::{Environment, EnvironmentEntry};
use super::*;
use crate::config::ShellKind;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    cmp::Ordering,
    ffi::{OsStr, OsString, c_void},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    ptr::{null, null_mut},
};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::{GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Globalization::CompareStringOrdinal,
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, SearchPathW},
    System::{
        Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW},
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

fn os_error(action: &str) -> ApiError {
    ApiError::new(
        ErrorCode::IoFailed,
        format!("{action}: {}", std::io::Error::last_os_error()),
    )
}
fn wide(value: &OsStr) -> Result<Vec<u16>, ApiError> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "Windows path or environment contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}
fn take_handle(handle: HANDLE, action: &str) -> Result<OwnedHandle, ApiError> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(os_error(action))
    } else {
        // Ownership is transferred exactly once from the successful Win32 call.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}
fn compare_keys(a: &[u16], b: &[u16]) -> Result<Ordering, ApiError> {
    let a_len = i32::try_from(a.len())
        .map_err(|_| ApiError::new(ErrorCode::InputLimit, "environment key too long"))?;
    let b_len = i32::try_from(b.len())
        .map_err(|_| ApiError::new(ErrorCode::InputLimit, "environment key too long"))?;
    match unsafe { CompareStringOrdinal(a.as_ptr(), a_len, b.as_ptr(), b_len, 1) } {
        1 => Ok(Ordering::Less),
        2 => Ok(Ordering::Equal),
        3 => Ok(Ordering::Greater),
        _ => Err(os_error("compare environment keys")),
    }
}
struct EnvironmentStrings(*mut u16);
impl Drop for EnvironmentStrings {
    fn drop(&mut self) {
        unsafe { FreeEnvironmentStringsW(self.0) };
    }
}
fn inherited_environment() -> Result<Environment, ApiError> {
    let raw = unsafe { GetEnvironmentStringsW() };
    if raw.is_null() {
        return Err(os_error("read inherited environment"));
    }
    let owned = EnvironmentStrings(raw);
    let mut result = Vec::new();
    let mut offset = 0;
    // GetEnvironmentStringsW owns a double-NUL-terminated UTF-16 block. Scan only
    // until its documented terminator and release with the matching Windows API.
    unsafe {
        while *owned.0.add(offset) != 0 {
            let start = offset;
            while *owned.0.add(offset) != 0 {
                offset += 1;
            }
            let entry = std::slice::from_raw_parts(owned.0.add(start), offset - start);
            let split = entry
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, v)| **v == b'=' as u16)
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::IoFailed,
                        "inherited environment has an invalid entry",
                    )
                })?;
            result.push((entry[..split].to_vec(), entry[split + 1..].to_vec()));
            offset += 1;
        }
    }
    Ok(result)
}
fn environment(config: &Config, extra: BTreeMap<String, String>) -> Result<Environment, ApiError> {
    let inherited = inherited_environment()?;
    let overrides = config
        .shell
        .env
        .iter()
        .chain(extra.iter())
        .map(|(k, v)| (k.encode_utf16().collect(), v.encode_utf16().collect()))
        .collect();
    let entries = windows_command::merge_environment(inherited, overrides, compare_keys)?;
    windows_command::environment_block(&entries)?; // Validate before OS creation.
    Ok(entries)
}
fn env_value<'a>(
    entries: &'a [(Vec<u16>, Vec<u16>)],
    key: &str,
) -> Result<Option<&'a [u16]>, ApiError> {
    let key: Vec<u16> = key.encode_utf16().collect();
    for (name, value) in entries {
        if compare_keys(name, &key)? == Ordering::Equal {
            return Ok(Some(value));
        }
    }
    Ok(None)
}
fn resolve_program(
    config: &Config,
    cwd: &std::path::Path,
    env: &[EnvironmentEntry],
) -> Result<PathBuf, ApiError> {
    let program = config.shell.executable.clone().unwrap_or_else(|| {
        PathBuf::from(match config.shell.kind {
            ShellKind::Bash => "bash.exe",
            ShellKind::Powershell => "powershell.exe",
        })
    });
    let resolved = if program.components().count() > 1 || program.is_absolute() {
        if program.is_absolute() {
            program
        } else {
            cwd.join(program)
        }
    } else {
        let mut path = env_value(env, "PATH")?
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::IoFailed,
                    "shell executable is not absolute and PATH is unavailable",
                )
            })?
            .to_vec();
        path.push(0);
        let program = wide(program.as_os_str())?;
        let extension: Vec<u16> = ".exe\0".encode_utf16().collect();
        let needed = unsafe {
            SearchPathW(
                path.as_ptr(),
                program.as_ptr(),
                extension.as_ptr(),
                0,
                null_mut(),
                null_mut(),
            )
        };
        if needed == 0 {
            return Err(os_error("resolve shell executable in configured PATH"));
        }
        let mut buffer = vec![0; needed as usize + 1];
        let written = unsafe {
            SearchPathW(
                path.as_ptr(),
                program.as_ptr(),
                extension.as_ptr(),
                buffer.len() as u32,
                buffer.as_mut_ptr(),
                null_mut(),
            )
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err(os_error("resolve shell executable"));
        }
        buffer.truncate(written as usize);
        PathBuf::from(OsString::from_wide(&buffer))
    };
    if !resolved.is_absolute() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "shell executable must resolve to an absolute path",
        ));
    }
    // The Windows inbox bash.exe is a WSL launcher, not the Git Bash adapter.
    if config.shell.kind == ShellKind::Bash
        && let Some(windir) = env_value(env, "WINDIR")?
    {
        let inbox = PathBuf::from(OsString::from_wide(windir))
            .join("System32")
            .join("bash.exe");
        let resolved_wide: Vec<u16> = resolved.as_os_str().encode_wide().collect();
        let inbox_wide: Vec<u16> = inbox.as_os_str().encode_wide().collect();
        if compare_keys(&resolved_wide, &inbox_wide)? == Ordering::Equal {
            return Err(ApiError::new(
                ErrorCode::ConfigInvalid,
                "Windows Bash requires Git Bash; the inbox WSL launcher is unsupported",
            )
            .hint("set shell.executable to Git Bash bash.exe"));
        }
    }
    Ok(resolved)
}
fn arguments(kind: ShellKind, command: &str) -> Vec<Vec<u16>> {
    let strings: Vec<String> = match kind {
        ShellKind::Bash => super::bash_arguments(command)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ShellKind::Powershell => {
            let encoded = STANDARD.encode(
                command
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-OutputFormat".into(),
                "Text".into(),
                "-EncodedCommand".into(),
                encoded,
            ]
        }
    };
    strings
        .into_iter()
        .map(|v| v.encode_utf16().collect())
        .collect()
}

/// Owns aligned backing allocation and calls Delete only after Initialize succeeds.
struct AttributeList {
    storage: Vec<usize>,
    initialized: bool,
}
impl AttributeList {
    fn new() -> Result<Self, ApiError> {
        let mut bytes = 0;
        unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes) };
        if bytes == 0 {
            return Err(os_error("size process attribute list"));
        }
        let mut value = Self {
            storage: vec![0; bytes.div_ceil(size_of::<usize>())],
            initialized: false,
        };
        if unsafe { InitializeProcThreadAttributeList(value.ptr(), 2, 0, &mut bytes) } == 0 {
            // Do not run Delete on uninitialized storage.
            return Err(os_error("initialize process attribute list"));
        }
        value.initialized = true;
        Ok(value)
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn set(&mut self, attribute: u32, handles: &[HANDLE]) -> Result<(), ApiError> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                attribute as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                null_mut(),
                null(),
            )
        } == 0
        {
            Err(os_error("set process attribute"))
        } else {
            Ok(())
        }
    }
}
impl Drop for AttributeList {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { DeleteProcThreadAttributeList(self.ptr()) };
        }
    }
}
fn create_job() -> Result<OwnedHandle, ApiError> {
    let job = take_handle(
        unsafe { CreateJobObjectW(null(), null()) },
        "create execution Job",
    )?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(os_error("set Job kill-on-close"));
    }
    Ok(job)
}
struct PipePair {
    parent: NamedPipeServer,
    child: OwnedHandle,
}
async fn output_pipe() -> Result<PipePair, ApiError> {
    let name = format!(r"\\.\pipe\shellboy-shell-{}", uuid::Uuid::new_v4());
    let parent = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .max_instances(1)
        .access_outbound(false)
        .access_inbound(true)
        .in_buffer_size(65536)
        .out_buffer_size(65536)
        .create(&name)
        .map_err(|error| {
            ApiError::new(ErrorCode::IoFailed, format!("create shell pipe: {error}"))
        })?;
    let name = wide(OsStr::new(&name))?;
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    // Child endpoints are synchronous. Only parent endpoints use overlapped I/O.
    let child = take_handle(
        unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                0,
                &security,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        },
        "open shell pipe endpoint",
    )?;
    timeout(Duration::from_secs(5), parent.connect())
        .await
        .map_err(|_| {
            ApiError::new(
                ErrorCode::IoFailed,
                "shell pipe connection deadline exceeded",
            )
        })?
        .map_err(|error| {
            ApiError::new(ErrorCode::IoFailed, format!("connect shell pipe: {error}"))
        })?;
    Ok(PipePair { parent, child })
}
struct NativeChild {
    job: OwnedHandle,
    process: OwnedHandle,
}
impl NativeChild {
    fn active(&self) -> Result<u32, ApiError> {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(os_error("query owned Job processes"));
        }
        Ok(info.ActiveProcesses)
    }
    fn terminate(&self) -> Result<(), ApiError> {
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            Err(os_error("terminate execution Job"))
        } else {
            Ok(())
        }
    }
    async fn wait(&self) -> Result<u32, ApiError> {
        loop {
            match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                        return Err(os_error("read shell exit code"));
                    }
                    return Ok(code);
                }
                WAIT_TIMEOUT => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => return Err(os_error("observe shell process handle")),
            }
        }
    }
    async fn confirm_empty(&self, deadline: Duration) -> Result<bool, ApiError> {
        let until = Instant::now() + deadline;
        loop {
            if self.active()? == 0 {
                return Ok(true);
            }
            if Instant::now() >= until {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl ShellSupervisor {
    pub(super) async fn spawn_windows(
        &self,
        execution_id: String,
        cwd: PathBuf,
        args: ShellToolArgs,
        env: BTreeMap<String, String>,
        initial: Option<Vec<u8>>,
        budget_size: usize,
    ) -> Result<ShellExecution, ApiError> {
        let entries = environment(&self.config, env)?;
        let executable = resolve_program(&self.config, &cwd, &entries)?;
        let exe = wide(executable.as_os_str())?;
        let mut line = windows_command::command_line(
            &exe[..exe.len() - 1],
            &arguments(self.config.shell.kind, &args.command),
        )?;
        let env_block = windows_command::environment_block(&entries)?;
        if !cwd.is_absolute() {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "shell cwd must be absolute",
            ));
        }
        let cwd = wide(cwd.as_os_str())?;
        let (stdin_parent, stdin_child) = super::windows_stdin::input_pipe().await?;
        let stdout = output_pipe().await?;
        let stderr = output_pipe().await?;
        let job = create_job()?;
        let jobs = [job.as_raw_handle()];
        let inherited = [
            stdin_child.as_raw_handle(),
            stdout.child.as_raw_handle(),
            stderr.child.as_raw_handle(),
        ];
        let mut attributes = AttributeList::new()?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &inherited)?;
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited[0];
        startup.StartupInfo.hStdOutput = inherited[1];
        startup.StartupInfo.hStdError = inherited[2];
        startup.lpAttributeList = attributes.ptr();
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
        // Attributes and their backing handle slices stay alive through this call.
        // Job assignment is part of creation, before the first instruction runs.
        let spawned_at = Instant::now();
        if unsafe {
            CreateProcessW(
                exe.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                env_block.as_ptr().cast::<c_void>(),
                cwd.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        } == 0
        {
            return Err(os_error(
                "create shell in execution Job (Windows 10+ required)",
            ));
        }
        let process = take_handle(info.hProcess, "own shell process handle")?;
        let thread = take_handle(info.hThread, "own shell thread handle")?;
        drop(thread);
        drop(attributes);
        // Parent must release its copies of child ends, otherwise EOF is lost.
        drop(stdin_child);
        drop(stdout.child);
        drop(stderr.child);
        let native = NativeChild { job, process };
        let stdin = Arc::new(Mutex::new(Some(ShellStdin::Windows(stdin_parent))));
        let (io, handle, rx) = IoSession::start(
            stdin,
            stdout.parent,
            stderr.parent,
            initial,
            args.stdin_open,
            budget_size,
            &self.config,
        );
        let (done_tx, done_rx) = watch::channel(None);
        let drain = Duration::from_millis(self.config.io.drain_timeout_ms);
        tokio::spawn(async move {
            let mut forced = false;
            let mut stopped_before_exit = false;
            let status = tokio::select! {
                biased;
                value=native.wait()=>value,
                _=io.stop.cancelled()=>{
                    stopped_before_exit=true;
                    forced=true;
                    if let Err(error)=native.terminate(){record_failure(&io.failure,FailureKind::Terminate,error.message);}
                    match timeout(drain,native.wait()).await {Ok(value)=>value,Err(_)=>Err(ApiError::new(ErrorCode::IoFailed,"shell process exit after Job termination is unconfirmed"))}
                }
            };
            if let Err(error) = &status {
                record_failure(&io.failure, FailureKind::ProcessWait, error.message.clone());
            }
            match native.active() {
                Ok(0) => {}
                Ok(_) => {
                    forced = true;
                    if let Err(error) = native.terminate() {
                        record_failure(&io.failure, FailureKind::Terminate, error.message);
                    }
                }
                Err(error) => {
                    record_failure(&io.failure, FailureKind::ActiveProcesses, error.message);
                    forced = true;
                    let _ = native.terminate();
                }
            }
            let confirmed = match native.confirm_empty(drain).await {
                Ok(value) => value,
                Err(error) => {
                    record_failure(&io.failure, FailureKind::GroupConfirm, error.message);
                    false
                }
            };
            let (state, reason, output_complete) = io.finish(drain, stopped_before_exit).await;
            let completion = ShellCompletion {
                execution_id,
                state,
                exit_code: status.ok().map(|v| v as i32),
                exit_signal: None,
                reason,
                forced,
                output_complete,
                group_cleanup_confirmed: confirmed,
                elapsed_ms: Some(completion_elapsed_ms(spawned_at)),
            };
            // Close the noninherited kill-on-close Job before publishing terminal
            // state, including any case where process-count confirmation failed.
            drop(native);
            let _ = done_tx.send(Some(completion));
        });
        Ok(ShellExecution {
            handle,
            events: rx,
            completion: done_rx,
        })
    }
}
