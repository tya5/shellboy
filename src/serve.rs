//! Terminal-free adapter over the shared owner bootstrap and shutdown path.
use crate::{
    owner_session,
    protocol::{ApiError, ErrorCode, PROTOCOL_VERSION},
};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf, process::ExitCode};
use tokio_util::sync::CancellationToken;

pub fn startup_error(error: ApiError) -> ExitCode {
    let value = json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error":error});
    let _ = writeln!(std::io::stderr().lock(), "{value}");
    ExitCode::FAILURE
}
fn emit(value: &Value) -> bool {
    let mut output = std::io::stdout().lock();
    writeln!(output, "{value}")
        .and_then(|()| output.flush())
        .is_ok()
}
#[cfg(unix)]
async fn signal(signals: &mut crate::os_signals::ServeSignals) {
    signals.recv().await;
}
#[cfg(not(unix))]
async fn signal(_: &mut ()) {
    std::future::pending::<()>().await;
}
async fn listener_failure(
    failures: &mut tokio::sync::watch::Receiver<Option<ApiError>>,
) -> ApiError {
    loop {
        if let Some(error) = failures.borrow().clone() {
            return error;
        }
        if failures.changed().await.is_err() {
            return ApiError::new(ErrorCode::IoFailed, "Owner listener ended unexpectedly");
        }
    }
}

/// Err is startup-only; once ready, a failed final result is stdout plus exit 1.
pub async fn run(config: Option<PathBuf>, data: PathBuf) -> Result<bool, ApiError> {
    #[cfg(unix)]
    let mut signals = crate::os_signals::ServeSignals::new().map_err(|_| {
        ApiError::new(
            ErrorCode::IoFailed,
            "Cannot observe owner termination signals",
        )
    })?;
    #[cfg(not(unix))]
    let mut signals = ();
    let cancel = CancellationToken::new();
    let startup = owner_session::initialize_reporting(config, Some(data), cancel.clone(), |_| {});
    tokio::pin!(startup);
    let prepared = tokio::select! { biased;
        _ = signal(&mut signals) => { cancel.cancel(); startup.await },
        result = &mut startup => result,
    }?;
    let Some(prepared) = prepared else {
        return Ok(emit(
            &json!({"protocol_version":PROTOCOL_VERSION,"event":"stopped",
            "state":"cancelled","ok":true,"data_dir":null,"instance_id":null}),
        ));
    };
    if prepared.owner.loaded.data_dir.to_str().is_none() {
        prepared.shutdown().await?;
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "serve requires a UTF-8 data directory for its JSON readiness record",
        ));
    }
    run_prepared(prepared, signal(&mut signals), emit).await
}

pub(crate) async fn run_prepared(
    prepared: owner_session::Prepared,
    stop_signal: impl std::future::Future<Output = ()>,
    emit: impl Fn(&Value) -> bool,
) -> Result<bool, ApiError> {
    let requested = prepared.control.owner_shutdown_signal();
    let mut failures = prepared.server.failures();
    let dispatcher = tokio::spawn(prepared.control.clone().serve(prepared.requests));
    let owner_session::Prepared {
        owner,
        control,
        server,
        ..
    } = prepared;
    let data = owner.loaded.data_dir.clone();
    let instance = owner.instance_id.clone();
    if !emit(
        &json!({"protocol_version":PROTOCOL_VERSION,"event":"ready","state":"ready",
        "data_dir":data,"instance_id":instance}),
    ) {
        let _ = owner_session::close(owner, Some(control), Some(server)).await;
        let _ = dispatcher.await;
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "Cannot write owner ready line",
        ));
    }
    let reason = tokio::select! { biased;
        _ = requested.cancelled() => Ok(()),
        _ = stop_signal => Ok(()),
        _ = control.stopping() => Ok(()),
        error = listener_failure(&mut failures) => Err(error),
    };
    control.freeze_owner_admission();
    let closed = owner_session::close(owner, Some(control), Some(server)).await;
    let dispatched = dispatcher
        .await
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Owner dispatcher failed"));
    let result = reason.and(closed).and(dispatched);
    let ok = result.is_ok();
    let written = emit(
        &json!({"protocol_version":PROTOCOL_VERSION,"event":"stopped",
        "state":if ok {"stopped"} else {"failed"},"ok":ok,"data_dir":data,
        "instance_id":instance,"error":result.err()}),
    );
    Ok(ok && written)
}

/// Attach to the selected owner, or attempt one detached `serve` launch.
/// Cancellation ends only this observation, including after the child acquires ownership.
#[doc(hidden)]
pub async fn attach_or_start(
    profile: &crate::config::LoadedConfig,
    cancel: &CancellationToken,
) -> Result<Option<crate::ipc::Welcome>, ApiError> {
    use tokio::time::{Duration, Instant};

    let data = &profile.data_dir;
    let result = async {
        if let Some(welcome) = crate::ipc::probe_selected(data).await? {
            return Ok(Some(welcome));
        }
        let lock_path = data.clone();
        let free = tokio::task::spawn_blocking(move || profile_is_unowned(&lock_path))
            .await
            .map_err(|_| attach_error("Owner lock inspection failed"))??;
        let mut child = if free {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            Some(spawn_detached(profile.config_path.as_deref(), data)?)
        } else {
            None
        };
        let readiness = async {
            loop {
                if let Some(welcome) = crate::ipc::probe_selected(data).await? {
                    return Ok(Some(welcome));
                }
                let exited = child
                    .as_mut()
                    .map(DetachedChild::exited)
                    .transpose()
                    .map_err(|_| attach_error("Cannot observe owner process"))?
                    .unwrap_or(false);
                if exited {
                    child = None;
                    let lock_path = data.clone();
                    let free = tokio::task::spawn_blocking(move || profile_is_unowned(&lock_path))
                        .await
                        .map_err(|_| attach_error("Owner lock inspection failed"))??;
                    if free {
                        // A successful concurrent launcher may have published readiness
                        // between the last probe and the losing child's exit.
                        return crate::ipc::probe_selected(data)
                            .await?
                            .map(Some)
                            .ok_or_else(|| {
                                attach_error("The detached owner exited before becoming ready")
                            });
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout_at(Instant::now() + Duration::from_secs(30), readiness)
            .await
            .map_err(|_| attach_error("The selected owner did not become ready in time"))?
    };
    tokio::select! { biased;
        _ = cancel.cancelled() => Ok(None),
        result = result => result,
    }
}

fn attach_error(message: &'static str) -> ApiError {
    ApiError::new(ErrorCode::IoFailed, message)
        .hint("Run shellboy serve explicitly for startup diagnostics")
}

fn profile_is_unowned(data: &std::path::Path) -> Result<bool, ApiError> {
    use std::io::ErrorKind;
    match std::fs::symlink_metadata(data) {
        Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(attach_error(
                        "The selected data directory belongs to another user",
                    ));
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        _ => {
            return Err(attach_error(
                "The selected data directory cannot be verified",
            ));
        }
    }
    let file = match crate::app::open_owner_lock(data, false) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(_) => {
            return Err(attach_error(
                "The selected owner lock cannot be opened safely",
            ));
        }
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => {
            fs2::FileExt::unlock(&file)
                .map_err(|_| attach_error("Cannot release owner lock inspection"))?;
            Ok(true)
        }
        Err(error) if error.kind() == fs2::lock_contended_error().kind() => Ok(false),
        Err(_) => Err(attach_error(
            "The selected owner lock state cannot be verified",
        )),
    }
}

#[cfg(unix)]
struct DetachedChild(tokio::process::Child);

#[cfg(windows)]
struct DetachedChild(std::os::windows::io::OwnedHandle);

impl DetachedChild {
    fn exited(&mut self) -> std::io::Result<bool> {
        #[cfg(unix)]
        return self.0.try_wait().map(|status| status.is_some());
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{
                Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
                System::Threading::WaitForSingleObject,
            };
            match unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => Ok(true),
                WAIT_TIMEOUT => Ok(false),
                _ => Err(std::io::Error::last_os_error()),
            }
        }
    }
}

fn spawn_detached(
    config: Option<&std::path::Path>,
    data: &std::path::Path,
) -> Result<DetachedChild, ApiError> {
    use std::ffi::OsStr;
    if !data.is_absolute() || data.to_str().is_none() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "serve requires an absolute UTF-8 data directory",
        ));
    }
    let executable = std::env::current_exe()
        .map_err(|_| attach_error("Cannot identify the Shellboy executable"))?;
    let mut args = vec![OsStr::new("--data-dir"), data.as_os_str()];
    if let Some(config) = config {
        args.extend([OsStr::new("--config"), config.as_os_str()]);
    }
    args.push(OsStr::new("serve"));
    spawn_owner(&executable, &args)
}

#[cfg(unix)]
fn spawn_owner(
    executable: &std::path::Path,
    args: &[&std::ffi::OsStr],
) -> Result<DetachedChild, ApiError> {
    use std::process::Stdio;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_detached_child(command.as_std_mut())?;
    command
        .spawn()
        .map(DetachedChild)
        .map_err(|_| attach_error("Cannot launch detached Shellboy owner"))
}

#[cfg(unix)]
fn configure_detached_child(command: &mut std::process::Command) -> Result<(), ApiError> {
    use std::os::unix::process::CommandExt;
    #[cfg(target_os = "macos")]
    let fd_limit = {
        let mut limit: libc::c_int = 0;
        let mut size = std::mem::size_of_val(&limit);
        // The system's per-process ceiling, not the caller's possibly lowered RLIMIT_NOFILE.
        if unsafe {
            libc::sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&mut limit as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
            || limit < 3
        {
            return Err(attach_error(
                "Cannot determine the owner descriptor boundary",
            ));
        }
        limit
    };
    // Mark instead of closing: Command's private exec error pipe still needs to
    // work until exec succeeds. These operations mutate only the forked child.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "macos")]
            for fd in 3..fd_limit {
                if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EBADF) {
                        return Err(error);
                    }
                }
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(windows)]
fn spawn_owner(
    executable: &std::path::Path,
    args: &[&std::ffi::OsStr],
) -> Result<DetachedChild, ApiError> {
    use std::{
        os::windows::{
            ffi::OsStrExt,
            io::{FromRawHandle, OwnedHandle},
        },
        ptr::null,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION,
        STARTUPINFOW,
    };
    let mut exe = executable.as_os_str().encode_wide().collect::<Vec<_>>();
    let args = args
        .iter()
        .map(|arg| arg.encode_wide().collect())
        .collect::<Vec<_>>();
    let mut line = crate::shell::windows_command::command_line(&exe, &args)?;
    exe.push(0);
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // No inherited handles (including standard streams) and no console.
    if unsafe {
        CreateProcessW(
            exe.as_ptr(),
            line.as_mut_ptr(),
            null(),
            null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            null(),
            null(),
            &startup,
            &mut info,
        )
    } == 0
    {
        return Err(attach_error("Cannot launch detached Shellboy owner"));
    }
    unsafe {
        drop(OwnedHandle::from_raw_handle(info.hThread));
        Ok(DetachedChild(OwnedHandle::from_raw_handle(info.hProcess)))
    }
}

#[cfg(all(test, unix))]
mod detached_tests {
    use super::configure_detached_child;
    use std::{
        fs::File,
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::{Child, Command, Stdio},
    };

    struct StopAndWait(Child);
    impl Drop for StopAndWait {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn detached_exec_does_not_retain_a_caller_lock_and_the_guard_reaps_the_child() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("caller.lock");
        let locked = File::create(&path).unwrap();
        let witness = File::options().read(true).write(true).open(&path).unwrap();
        fs2::FileExt::try_lock_exclusive(&locked).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&witness).is_err());

        let mut command = Command::new("/bin/cat");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let fd = locked.as_raw_fd();
        // Simulate a caller's inheritable lock without changing another test's fork.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        configure_detached_child(&mut command).unwrap();
        let mut child = StopAndWait(command.spawn().unwrap());
        let pid = child.0.id() as libc::pid_t;
        assert!(child.0.try_wait().unwrap().is_none());
        drop(locked);
        fs2::FileExt::try_lock_exclusive(&witness)
            .expect("live detached child must not retain the lock");
        drop(child);
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
