//! Stateless descriptor headroom at Unix shell start admission (#450).
//!
//! Every value is read live at admission: nothing is reserved, queued or
//! counted between starts. The check runs under the native ownership lock, so
//! concurrent shell starts observe each other's descriptors.
use crate::protocol::{ApiError, ErrorCode};

use super::DESCRIPTOR_BUDGET_REASON as REASON;

/// Upper bound on descriptors the owner creates inside one native spawn after
/// admission (the lifeline socket pairs already exist and are counted as open):
/// - std `Stdio::piped()` for stdin, stdout and stderr: three pipes, 6 FDs;
/// - std's fork-path exec-error pipe (used because a pre_exec hook is set): 2 FDs;
/// - Tokio's reaper: a Linux pidfd (1 FD), or the process-wide SIGCHLD
///   self-pipe pair created on first registration (2 FDs); the larger is used.
pub(crate) const SPAWN_ADD: usize = 6 + 2 + 2;

/// Descriptors kept free for IPC: every admitted connection holds one socket.
pub(crate) const IPC_RESERVE: usize = crate::ipc::MAX_CONNECTIONS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DescriptorUse {
    pub open: usize,
    pub limit: usize,
}

pub(crate) fn admit(observed: Result<DescriptorUse, std::io::Error>) -> Result<(), ApiError> {
    match observed {
        Ok(used) if used.open + SPAWN_ADD + IPC_RESERVE <= used.limit => Ok(()),
        Ok(used) => Err(refusal(
            format!(
                "Shell start refused: {} of {} file descriptors are open and {} stay reserved for IPC",
                used.open, used.limit, IPC_RESERVE
            ),
            Some(used),
        )),
        Err(error) => Err(refusal(
            format!("Shell start refused: open file descriptors cannot be counted: {error}"),
            None,
        )),
    }
}

fn refusal(message: String, used: Option<DescriptorUse>) -> ApiError {
    ApiError::new(ErrorCode::InvalidState, message)
        .details(serde_json::json!({
            "reason": REASON,
            "open": used.map(|used| used.open),
            "limit": used.map(|used| used.limit),
            "reserve": SPAWN_ADD + IPC_RESERVE,
        }))
        .hint("List background shells with shellboy shell list --state running, stop unneeded ones with shellboy shell stop --execution EXECUTION_ID, then retry.")
}

pub(crate) fn observe() -> Result<DescriptorUse, std::io::Error> {
    Ok(DescriptorUse {
        open: open_descriptors()?,
        limit: soft_limit()?,
    })
}

fn soft_limit() -> Result<usize, std::io::Error> {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limits is initialized and writable.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let soft = usize::try_from(limits.rlim_cur).unwrap_or(usize::MAX);
    #[cfg(target_os = "macos")]
    {
        let mut per_process: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        // SAFETY: the name is NUL-terminated and the output buffer matches size.
        let status = unsafe {
            libc::sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&mut per_process as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(soft.min(usize::try_from(per_process).unwrap_or(0)))
    }
    #[cfg(not(target_os = "macos"))]
    Ok(soft)
}

#[cfg(target_os = "macos")]
fn open_descriptors() -> Result<usize, std::io::Error> {
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    // SAFETY: a null buffer asks only for the size needed for the current table.
    let needed = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if needed <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Room for descriptors opened by other owner threads between the two calls;
    // a full buffer is retried with a larger one instead of being trusted.
    let mut capacity = needed as usize / entry + SPAWN_ADD;
    loop {
        let mut entries: Vec<libc::proc_fdinfo> = Vec::with_capacity(capacity);
        let bytes = libc::c_int::try_from(capacity * entry)
            .map_err(|_| std::io::Error::other("descriptor table too large"))?;
        // SAFETY: the buffer has `bytes` writable bytes of proc_fdinfo entries.
        let listed = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDLISTFDS,
                0,
                entries.as_mut_ptr().cast(),
                bytes,
            )
        };
        // proc_pidinfo reports errors as 0, never -1; fd 0 is always open here.
        if listed <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        if listed < bytes {
            return Ok(listed as usize / entry);
        }
        capacity *= 2;
    }
}

#[cfg(target_os = "linux")]
fn open_descriptors() -> Result<usize, std::io::Error> {
    let mut count = 0usize;
    for entry in std::fs::read_dir("/proc/self/fd")? {
        entry?;
        count += 1;
    }
    // The listing includes the directory descriptor read_dir holds open.
    Ok(count.saturating_sub(1))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_descriptors() -> Result<usize, std::io::Error> {
    Err(std::io::Error::other(
        "open descriptor counting is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_holds_exactly_up_to_the_reserved_headroom() {
        let limit = 1024;
        let boundary = limit - SPAWN_ADD - IPC_RESERVE;
        assert!(
            admit(Ok(DescriptorUse {
                open: boundary,
                limit
            }))
            .is_ok()
        );
        let refused = admit(Ok(DescriptorUse {
            open: boundary + 1,
            limit,
        }))
        .unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidState);
        let details = refused.details.unwrap();
        assert_eq!(details["reason"], REASON);
        assert_eq!(details["open"], boundary + 1);
        assert_eq!(details["limit"], limit);
        assert_eq!(details["reserve"], SPAWN_ADD + IPC_RESERVE);
        assert!(refused.hint.is_some());
    }

    #[test]
    fn an_uncountable_table_is_refused_rather_than_assumed() {
        let refused = admit(Err(std::io::Error::other("fixture"))).unwrap_err();
        let details = refused.details.unwrap();
        assert_eq!(details["reason"], REASON);
        assert!(details["open"].is_null());
    }

    #[test]
    fn live_observation_reads_this_platforms_table_and_limit() {
        let observed = observe().unwrap();
        // stdin/stdout/stderr or the test harness keep at least one descriptor open.
        assert!(observed.open > 0);
        assert!(observed.limit > 0);
    }
}
