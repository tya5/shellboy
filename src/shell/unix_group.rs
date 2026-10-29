//! Unix group signaling and finite cleanup, owned by the live supervisor.
use super::{Duration, Instant};

/// At most one entry per live native supervisor task. Removal on task completion
/// or abort bounds lifetime; group ids never come from persistent state.
pub(super) struct OwnedGroup {
    groups: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<i32>>>,
    pgid: i32,
    confirmed_gone: bool,
}
impl OwnedGroup {
    pub(super) fn new(
        groups: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<i32>>>,
        pgid: i32,
    ) -> Self {
        Self {
            groups,
            pgid,
            confirmed_gone: false,
        }
    }
    pub(super) fn complete(mut self, confirmed_gone: bool) {
        self.confirmed_gone = confirmed_gone;
        // Drop now, before output drain awaits: do not retain a stale PGID
        // after disappearance or defer the last unconfirmed KILL until later.
    }
}
impl Drop for OwnedGroup {
    fn drop(&mut self) {
        if !self.confirmed_gone {
            let _ = signal_group(self.pgid, libc::SIGKILL);
        }
        self.groups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.pgid);
    }
}

pub(super) fn signal_group(pgid: i32, signal: i32) -> std::io::Result<()> {
    // Only the group number spawned by this live supervisor reaches here.
    // It is never restored from storage; probe/signal is not an atomic identity check.
    let result = unsafe { libc::kill(-pgid, signal) };
    if result == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}
fn group_exists(pgid: i32) -> std::io::Result<bool> {
    let result = unsafe { libc::kill(-pgid, 0) };
    if result == 0 {
        Ok(true)
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }
}
pub(super) async fn cleanup_group(
    pgid: i32,
    kill_at: Option<Instant>,
    grace: Duration,
    drain: Duration,
) -> (bool, bool) {
    cleanup_with(pgid, kill_at, grace, drain, group_exists, signal_group).await
}

async fn cleanup_with(
    pgid: i32,
    kill_at: Option<Instant>,
    grace: Duration,
    drain: Duration,
    mut exists: impl FnMut(i32) -> std::io::Result<bool>,
    mut signal: impl FnMut(i32, i32) -> std::io::Result<()>,
) -> (bool, bool) {
    if !exists(pgid).unwrap_or(true) {
        return (true, false);
    }
    let _ = signal(pgid, libc::SIGTERM);
    let deadline = kill_at.unwrap_or_else(|| Instant::now() + grace);
    while Instant::now() < deadline {
        if !exists(pgid).unwrap_or(true) {
            return (true, false);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The last loop probe may precede its sleep and the grace deadline. Avoid
    // an unconditional delayed signal after known disappearance; this syscall
    // pair still cannot provide atomic, generation-bound PGID ownership.
    if !exists(pgid).unwrap_or(true) {
        return (true, false);
    }
    let _ = signal(pgid, libc::SIGKILL);
    let confirm = Instant::now() + drain.min(Duration::from_millis(250));
    while Instant::now() < confirm {
        if !exists(pgid).unwrap_or(true) {
            return (true, true);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (!exists(pgid).unwrap_or(true), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, io};

    #[tokio::test]
    async fn disappearance_at_grace_boundary_skips_delayed_kill() {
        let mut probes = VecDeque::from([true, false]);
        let mut signals = Vec::new();
        let outcome = cleanup_with(
            123,
            Some(Instant::now()),
            Duration::from_secs(1),
            Duration::ZERO,
            |_| Ok(probes.pop_front().expect("unexpected extra probe")),
            |pgid, signal| {
                assert_eq!(pgid, 123);
                signals.push(signal);
                Ok(())
            },
        )
        .await;
        assert_eq!(outcome, (true, false));
        assert_eq!(signals, [libc::SIGTERM]);
        assert!(probes.is_empty());
    }

    #[tokio::test]
    async fn group_present_at_grace_boundary_is_killed_then_confirmed() {
        let mut probes = VecDeque::from([true, true, false]);
        let mut signals = Vec::new();
        let outcome = cleanup_with(
            123,
            Some(Instant::now()),
            Duration::from_secs(1),
            Duration::ZERO,
            |_| Ok(probes.pop_front().expect("unexpected extra probe")),
            |_, signal| {
                signals.push(signal);
                Ok(())
            },
        )
        .await;
        assert_eq!(outcome, (true, true));
        assert_eq!(signals, [libc::SIGTERM, libc::SIGKILL]);
        assert!(probes.is_empty());
    }

    #[tokio::test]
    async fn permission_error_never_claims_group_disappearance() {
        let mut probes = 0;
        let mut signals = Vec::new();
        let outcome = cleanup_with(
            123,
            Some(Instant::now()),
            Duration::from_secs(1),
            Duration::ZERO,
            |_| {
                probes += 1;
                Err(io::Error::from_raw_os_error(libc::EPERM))
            },
            |_, signal| {
                signals.push(signal);
                Err(io::Error::from_raw_os_error(libc::EPERM))
            },
        )
        .await;
        // forced records a KILL attempt, not successful permission or removal.
        assert_eq!(outcome, (false, true));
        assert_eq!(signals, [libc::SIGTERM, libc::SIGKILL]);
        assert_eq!(probes, 3);
    }

    #[tokio::test]
    async fn already_absent_group_does_not_receive_any_signal() {
        let outcome = cleanup_with(
            123,
            Some(Instant::now()),
            Duration::from_secs(1),
            Duration::ZERO,
            |_| Ok(false),
            |_, _| panic!("an absent group must not be signaled"),
        )
        .await;
        assert_eq!(outcome, (true, false));
    }
}
