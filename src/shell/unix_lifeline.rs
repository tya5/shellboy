//! One native guardian per execution; no runtime worker or persisted PID authority.
use std::{
    collections::BTreeMap,
    ffi::{CString, OsString},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream, process::ExitStatusExt},
    },
    process::ExitStatus,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};

struct Exec {
    paths: Vec<CString>,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
    fallback: Vec<Vec<*const libc::c_char>>,
    _args: Vec<CString>,
    _env: Vec<CString>,
}
// Pointers refer only to the owned, immutable CString allocations.
unsafe impl Send for Exec {}
unsafe impl Sync for Exec {}
fn cstring(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "shell launch contains NUL"))
}
impl Exec {
    fn new(command: &Command) -> io::Result<Self> {
        let cmd = command.as_std();
        let mut env: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
        for (key, value) in cmd.get_envs() {
            if let Some(value) = value {
                env.insert(key.into(), value.into());
            } else {
                env.remove(key);
            }
        }
        let program = cmd.get_program();
        let paths = if program.as_bytes().contains(&b'/') {
            vec![cstring(program.as_bytes())?]
        } else {
            std::env::split_paths(
                env.get(std::ffi::OsStr::new("PATH"))
                    .map(OsString::as_os_str)
                    .unwrap_or(std::ffi::OsStr::new("/usr/bin:/bin")),
            )
            .map(|directory| cstring(directory.join(program).as_os_str().as_bytes()))
            .collect::<io::Result<Vec<_>>>()?
        };
        let args = std::iter::once(program)
            .chain(cmd.get_args())
            .map(|arg| cstring(arg.as_bytes()))
            .collect::<io::Result<Vec<_>>>()?;
        let env = env
            .into_iter()
            .map(|(key, value)| {
                let mut bytes = key.as_bytes().to_vec();
                if bytes.contains(&b'=') {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid shell environment name",
                    ));
                }
                bytes.push(b'=');
                bytes.extend_from_slice(value.as_bytes());
                cstring(&bytes)
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut argv: Vec<_> = args.iter().map(|v| v.as_ptr()).collect();
        argv.push(std::ptr::null());
        let mut envp: Vec<_> = env.iter().map(|v| v.as_ptr()).collect();
        envp.push(std::ptr::null());
        let fallback = paths
            .iter()
            .map(|path| {
                let mut argv = vec![c"/bin/sh".as_ptr(), path.as_ptr()];
                argv.extend(args.iter().skip(1).map(|v| v.as_ptr()));
                argv.push(std::ptr::null());
                argv
            })
            .collect();
        Ok(Self {
            paths,
            argv,
            envp,
            fallback,
            _args: args,
            _env: env,
        })
    }
    unsafe fn run(&self, errors: i32, mask: &libc::sigset_t) -> ! {
        unsafe {
            libc::sigprocmask(libc::SIG_SETMASK, mask, std::ptr::null_mut());
            let mut error = libc::ENOENT;
            let mut denied = false;
            for (index, path) in self.paths.iter().enumerate() {
                libc::execve(path.as_ptr(), self.argv.as_ptr(), self.envp.as_ptr());
                error = errno();
                if error == libc::ENOEXEC {
                    libc::execve(
                        c"/bin/sh".as_ptr(),
                        self.fallback[index].as_ptr(),
                        self.envp.as_ptr(),
                    );
                    error = errno();
                    break;
                }
                if error == libc::EACCES {
                    denied = true;
                } else if !matches!(error, libc::ENOENT | libc::ENOTDIR) {
                    break;
                }
            }
            if denied && matches!(error, libc::ENOENT | libc::ENOTDIR) {
                error = libc::EACCES;
            }
            write_int(errors, error);
            libc::_exit(127)
        }
    }
}

pub(super) struct Prepared {
    life: UnixStream,
    status: UnixStream,
    errors: UnixStream,
}
fn private_pair() -> io::Result<(UnixStream, UnixStream)> {
    fn private(stream: UnixStream) -> io::Result<UnixStream> {
        if stream.as_raw_fd() >= 3 {
            return Ok(stream);
        }
        let fd = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(UnixStream::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
    let (a, b) = UnixStream::pair()?;
    Ok((private(a)?, private(b)?))
}
impl Prepared {
    pub fn configure(command: &mut Command) -> io::Result<Self> {
        let exec = Exec::new(command)?;
        let (life, guard_life) = private_pair()?;
        let (status, guard_status) = private_pair()?;
        let (errors, child_errors) = private_pair()?;
        let limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        if !(3..=i32::MAX as libc::c_long).contains(&limit) {
            return Err(io::Error::other("cannot bound inherited descriptor table"));
        }
        let limit = (limit as i32)
            .max(guard_life.as_raw_fd() + 1)
            .max(guard_status.as_raw_fd() + 1)
            .max(child_errors.as_raw_fd() + 1);
        // pre_exec runs after stdio/cwd/process_group setup. Its guardian branch
        // never returns into Rust; its shell branch execs or reports errno/_exit.
        unsafe {
            command.pre_exec(move || {
                let mut mask = std::mem::zeroed();
                let mut blocked = std::mem::zeroed();
                libc::sigfillset(&mut blocked);
                if libc::sigprocmask(libc::SIG_SETMASK, &blocked, &mut mask) != 0 {
                    return Err(io::Error::last_os_error());
                }
                let child = libc::fork();
                if child < 0 {
                    return Err(io::Error::last_os_error());
                }
                if child == 0 {
                    exec.run(child_errors.as_raw_fd(), &mask);
                }
                guard(
                    child,
                    guard_life.as_raw_fd(),
                    guard_status.as_raw_fd(),
                    limit,
                )
            });
        }
        Ok(Self {
            life,
            status,
            errors,
        })
    }
    pub fn attached(mut self, child: Child) -> io::Result<NativeChild> {
        let attached = (|| {
            let mut bytes = [0; 4];
            let count = loop {
                match self.errors.read(&mut bytes) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => break result?,
                }
            };
            if count != 0 {
                self.errors.read_exact(&mut bytes[count..])?;
                return Err(io::Error::from_raw_os_error(i32::from_ne_bytes(bytes)));
            }
            self.status.set_nonblocking(true)?;
            tokio::net::UnixStream::from_std(self.status)
        })();
        match attached {
            Ok(status) => Ok(NativeChild {
                child,
                life: Some(self.life),
                status,
                bytes: [0; 4],
                received: 0,
                acknowledged: false,
            }),
            Err(error) => {
                if let Some(group) = child.id() {
                    unsafe {
                        libc::kill(-(group as i32), libc::SIGKILL);
                    }
                }
                Err(error)
            }
        }
    }
}
pub(super) struct NativeChild {
    pub child: Child,
    life: Option<UnixStream>,
    status: tokio::net::UnixStream,
    bytes: [u8; 4],
    received: usize,
    acknowledged: bool,
}
impl NativeChild {
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        while self.received < 4 {
            let n = unsafe {
                libc::read(
                    self.status.as_raw_fd(),
                    self.bytes[self.received..].as_mut_ptr().cast(),
                    4 - self.received,
                )
            };
            let n = if n >= 0 {
                n as usize
            } else {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(error);
                }
                self.status.read(&mut self.bytes[self.received..]).await?
            };
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Unix guardian closed before shell wait status",
                ));
            }
            self.received += n;
        }
        if !self.acknowledged {
            if !unsafe { write_int(self.status.as_raw_fd(), 1) } {
                return Err(io::Error::last_os_error());
            }
            self.acknowledged = true;
        }
        Ok(ExitStatus::from_raw(i32::from_ne_bytes(self.bytes)))
    }
    pub async fn reap_shell(&mut self) -> io::Result<()> {
        let mut ack = [0; 4];
        self.status.read_exact(&mut ack).await?;
        if i32::from_ne_bytes(ack) != 1 {
            return Err(io::Error::other("invalid Unix reap acknowledgment"));
        }
        Ok(())
    }
    pub fn release(&mut self) {
        self.life.take();
    }
}
impl Drop for NativeChild {
    fn drop(&mut self) {
        // Before Tokio can kill/reap the guardian, signal its reserved group.
        // A reaped guardian has no id: never signal an old number in that case.
        if let Some(group) = self.child.id() {
            unsafe {
                libc::kill(-(group as i32), libc::SIGKILL);
            }
        }
        self.life.take();
    }
}
extern "C" fn child_signal(_: libc::c_int) {}
unsafe fn errno() -> i32 {
    // last_os_error only reads thread-local errno; it does not allocate.
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
unsafe fn write_int(fd: i32, value: i32) -> bool {
    unsafe {
        let bytes = value.to_ne_bytes();
        let mut sent = 0;
        while sent < bytes.len() {
            let n = libc::write(fd, bytes[sent..].as_ptr().cast(), bytes.len() - sent);
            if n > 0 {
                sent += n as usize;
            } else if n < 0 && errno() == libc::EINTR {
                continue;
            } else {
                return false;
            }
        }
        true
    }
}
// Inclusive lower bound, including stderr when first == 2.
unsafe fn close_from(first: u32, limit: i32) {
    unsafe {
        #[cfg(target_os = "linux")]
        if libc::syscall(libc::SYS_close_range, first, u32::MAX, 0u32) == 0 {
            return;
        }
        for fd in first as i32..limit {
            libc::close(fd);
        }
    }
}
// A short-lived child creates a private group which the guardian can join.
// Its setup pipe prevents an orphan if setup is interrupted. The guardian PID
// remains reserved for the model group, outside both model and owner groups.
unsafe fn private_group(limit: i32) -> bool {
    unsafe {
        let mut pipe = [-1; 2];
        if libc::pipe(pipe.as_mut_ptr()) != 0 {
            return false;
        }
        let anchor = libc::fork();
        if anchor == 0 {
            if libc::dup2(pipe[0], 0) < 0 {
                libc::_exit(126);
            }
            close_from(1, limit);
            let mut byte = 0u8;
            while libc::read(0, (&mut byte as *mut u8).cast(), 1) < 0 && errno() == libc::EINTR {}
            libc::_exit(0);
        }
        libc::close(pipe[0]);
        let joined =
            anchor > 0 && libc::setpgid(anchor, anchor) == 0 && libc::setpgid(0, anchor) == 0;
        libc::close(pipe[1]);
        if anchor > 0 {
            while libc::waitpid(anchor, std::ptr::null_mut(), 0) < 0 && errno() == libc::EINTR {}
        }
        joined
    }
}
unsafe fn guard(child: i32, life: i32, status: i32, limit: i32) -> ! {
    unsafe {
        let group = libc::getpid();
        if libc::dup2(life, 0) < 0 || libc::dup2(status, 1) < 0 || !private_group(limit) {
            libc::kill(-group, libc::SIGKILL);
            libc::_exit(126);
        }
        // fd2 (shell stderr) is closed too: its EOF must not depend on release.
        // Do not keep stdout, owner locks, other lifelines or std's exec-error
        // pipe alive. The execing shell alone must decide spawn success/failure.
        close_from(2, limit);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = child_signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) != 0 {
            libc::kill(-group, libc::SIGKILL);
            libc::_exit(126);
        }
        let mut waiting: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut waiting);
        libc::sigdelset(&mut waiting, libc::SIGCHLD);
        let mut sent = false;
        let mut reaped = false;
        loop {
            if !sent {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                let result = libc::waitid(
                    libc::P_PID,
                    child as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                );
                if result == 0 && info.si_pid() == child {
                    let code = match info.si_code {
                        libc::CLD_EXITED => info.si_status() << 8,
                        libc::CLD_KILLED => info.si_status(),
                        libc::CLD_DUMPED => info.si_status() | 0x80,
                        _ => break,
                    };
                    if !write_int(1, code) {
                        break;
                    }
                    sent = true;
                } else if result < 0 && errno() != libc::EINTR {
                    break;
                }
            }
            let mut read: libc::fd_set = std::mem::zeroed();
            libc::FD_ZERO(&mut read);
            libc::FD_SET(0, &mut read);
            if sent && !reaped {
                libc::FD_SET(1, &mut read);
            }
            let ready = libc::pselect(
                2,
                &mut read,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                &waiting,
            );
            if ready < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                break;
            }
            if libc::FD_ISSET(0, &read) {
                break;
            }
            // Do not make the model shell disappear before its owner has received
            // the natural status: a later stop must not relabel a reaped exit.
            if sent && !reaped && libc::FD_ISSET(1, &read) {
                let mut ack = 0u8;
                if libc::read(1, (&mut ack as *mut u8).cast(), 1) != 1 {
                    break;
                }
                while libc::waitpid(child, std::ptr::null_mut(), 0) < 0 {
                    if errno() != libc::EINTR {
                        libc::kill(-group, libc::SIGKILL);
                        libc::_exit(126);
                    }
                }
                reaped = true;
                if !write_int(1, 1) {
                    break;
                }
            }
        }
        libc::kill(-group, libc::SIGKILL);
        if !reaped && libc::getpgid(child) == group {
            while libc::waitpid(child, std::ptr::null_mut(), 0) < 0 && errno() == libc::EINTR {}
        }
        libc::_exit(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn natural_shell_exit_drains_stderr_to_eof_before_guardian_release() {
        crate::test_watchdog::watchdog("native stderr EOF before guardian release", async {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", "printf stdout-tail; printf stderr-tail >&2"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .process_group(0);
            let mut native = tokio::task::spawn_blocking(move || {
                let prepared = Prepared::configure(&mut command).unwrap();
                let child = command.spawn().unwrap();
                // Match the supervisor: release parent pre_exec FD copies first.
                drop(command);
                prepared.attached(child).unwrap()
            })
            .await
            .unwrap();
            let mut stderr = native.child.stderr.take().unwrap();
            let mut stdout = native.child.stdout.take().unwrap();
            assert!(native.wait().await.unwrap().success());
            native.reap_shell().await.unwrap();
            assert!(native.child.try_wait().unwrap().is_none());
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, b"stderr-tail");
            assert!(
                native.child.try_wait().unwrap().is_none(),
                "EOF cannot rely on guardian exit"
            );
            native.release();
            assert!(native.child.wait().await.unwrap().success());
            bytes.clear();
            stdout.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, b"stdout-tail");
        })
        .await
        .unwrap();
    }
}
