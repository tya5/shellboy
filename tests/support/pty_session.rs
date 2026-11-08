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
