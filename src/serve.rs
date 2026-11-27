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
                    .map(tokio::process::Child::try_wait)
                    .transpose()
                    .map_err(|_| attach_error("Cannot observe owner process"))?
                    .flatten()
                    .is_some();
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
    let path = data.join("owner.lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
        Err(_) => {
            return Err(attach_error(
                "The selected owner lock cannot be opened safely",
            ));
        }
    };
    if !file
        .metadata()
        .is_ok_and(|m| m.is_file() && !m.is_symlink())
    {
        return Err(attach_error(
            "The selected owner lock is not a regular file",
        ));
    }
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

fn spawn_detached(
    config: Option<&std::path::Path>,
    data: &std::path::Path,
) -> Result<tokio::process::Child, ApiError> {
    use std::process::Stdio;
    if !data.is_absolute() || data.to_str().is_none() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "serve requires an absolute UTF-8 data directory",
        ));
    }
    let executable = std::env::current_exe()
        .map_err(|_| attach_error("Cannot identify the Shellboy executable"))?;
    let mut command = tokio::process::Command::new(executable);
    command.arg("--data-dir").arg(data);
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    command
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    // setsid is async-signal-safe and prevents terminal hangup from following the child.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(
        windows_sys::Win32::System::Threading::DETACHED_PROCESS
            | windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
    );
    command
        .spawn()
        .map_err(|_| attach_error("Cannot launch detached Shellboy owner"))
}
