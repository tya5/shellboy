//! Give an interactive child its disposable PTY as its controlling terminal on Unix.
#[cfg(unix)]
pub fn isolate_child(command: &mut tokio::process::Command) {
    // SAFETY: the child only calls async-signal-safe libc operations before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1
                || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn isolate_child(_command: &mut tokio::process::Command) {}

/// Own the real serve process before starting a PTY client. The profile is
/// disposable; a failed test must reap its owner before removing the profile.
#[cfg(unix)]
pub struct ServeFixture(std::process::Child);

#[cfg(unix)]
impl ServeFixture {
    pub async fn start(
        binary: &std::ffi::OsStr,
        cwd: &std::path::Path,
        config: &std::path::Path,
        data: &std::path::Path,
    ) -> Self {
        use shellboy::protocol::{Command, ThreadListArgs};
        use std::process::{Command as Process, Stdio};
        let mut owner = Self(
            Process::new(binary)
                .arg("--config")
                .arg(config)
                .arg("--data-dir")
                .arg(data)
                .arg("serve")
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start disposable owner"),
        );
        crate::test_watchdog::watchdog("disposable PTY owner readiness", async {
            loop {
                assert!(
                    owner.0.try_wait().unwrap().is_none(),
                    "disposable owner exited before readiness"
                );
                if shellboy::ipc::call(
                    data,
                    Command::ThreadList(ThreadListArgs::default()),
                    None,
                    None,
                )
                .await
                .is_ok_and(|response| response.ok)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        owner
    }
}

#[cfg(unix)]
impl Drop for ServeFixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(all(test, unix))]
#[tokio::test]
async fn dropping_the_disposable_owner_reaps_the_real_serve_child() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let data = root.path().join("data");
    std::fs::write(&config, "").unwrap();
    let owner = ServeFixture::start(
        std::ffi::OsStr::new(env!("CARGO_BIN_EXE_shellboy")),
        root.path(),
        &config,
        &data,
    )
    .await;
    let pid = owner.0.id() as libc::pid_t;
    drop(owner);
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    let lock = std::fs::File::options()
        .read(true)
        .write(true)
        .open(data.join("owner.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
}
