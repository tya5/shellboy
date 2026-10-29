//! Owner-only OS termination observation; no I/O or queue wait on the UI thread.
use crate::diagnostics::{Diagnostics, ShutdownBoundary, ShutdownScope, ShutdownStage};
use std::{
    io,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;
const LOST: u32 = 4;
#[cfg(windows)]
const GRACE: Duration = Duration::from_secs(4);
#[cfg(not(windows))]
const GRACE: Duration = Duration::from_secs(5);
#[derive(Clone, Copy)]
#[repr(u8)]
enum Source {
    Term = 1,
    Hup = 2,
    ConsoleClose = 3,
    ConsoleLogoff = 4,
    ConsoleShutdown = 5,
}
impl Source {
    fn decode(value: u32) -> Option<Self> {
        match value as u8 {
            1 => Some(Self::Term),
            2 => Some(Self::Hup),
            3 => Some(Self::ConsoleClose),
            4 => Some(Self::ConsoleLogoff),
            5 => Some(Self::ConsoleShutdown),
            _ => None,
        }
    }
    fn stage(self) -> ShutdownStage {
        match self {
            Self::Term => ShutdownStage::SignalTerm,
            Self::Hup => ShutdownStage::SignalHup,
            Self::ConsoleClose => ShutdownStage::ConsoleClose,
            Self::ConsoleLogoff => ShutdownStage::ConsoleLogoff,
            Self::ConsoleShutdown => ShutdownStage::ConsoleShutdown,
        }
    }
}
struct Shared {
    // low two bits: observed OS signal count (saturates at 2); bit2: terminal
    // lost; bytes1/2: first/second OS source. One atomic snapshot prevents races.
    state: AtomicU32,
    forced: AtomicBool,
    logged: AtomicU8,
    notify: Notify,
    done: CancellationToken,
    diagnostics: OnceLock<Diagnostics>,
    started: Instant,
}
impl Shared {
    fn new() -> Self {
        Self {
            state: AtomicU32::new(0),
            forced: AtomicBool::new(false),
            logged: AtomicU8::new(0),
            notify: Notify::new(),
            done: CancellationToken::new(),
            diagnostics: OnceLock::new(),
            started: Instant::now(),
        }
    }
    fn signal(&self, source: Source) {
        let _ = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                let count = old & 3;
                Some(if count == 0 {
                    (old & !3) | 1 | ((source as u32) << 8)
                } else if count == 1 {
                    (old & !3) | 2 | ((source as u32) << 16)
                } else {
                    old
                })
            });
        self.notify.notify_one();
    }
    fn lost(&self) {
        self.state.fetch_or(LOST, Ordering::AcqRel);
        self.notify.notify_one();
    }
    fn requested(&self) -> bool {
        self.state.load(Ordering::Acquire) & 7 != 0
    }
    fn record(&self, bit: u8, stage: ShutdownStage) {
        let Some(diagnostics) = self.diagnostics.get() else {
            return;
        };
        if self.logged.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            diagnostics.record_shutdown(
                ShutdownScope::Signal,
                stage,
                ShutdownBoundary::Begin,
                u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            );
        }
    }
    fn flush(&self) {
        let state = self.state.load(Ordering::Acquire);
        if state & LOST != 0 {
            self.record(1, ShutdownStage::TerminalLost);
        }
        if state & 3 >= 1
            && let Some(source) = Source::decode(state >> 8)
        {
            self.record(2, source.stage());
        }
        if state & 3 >= 2
            && let Some(source) = Source::decode(state >> 16)
        {
            self.record(4, source.stage());
        }
    }
    async fn force(&self, deadline: bool) {
        self.flush();
        self.record(
            8,
            if deadline {
                ShutdownStage::SignalDeadline
            } else {
                ShutdownStage::SignalForce
            },
        );
        self.forced.store(true, Ordering::Release);
        self.notify.notify_waiters();
        tokio::select! {biased;_=self.done.cancelled()=>{},_=tokio::time::sleep(Duration::from_millis(100))=>std::process::exit(1)}
    }
}
#[cfg(unix)]
struct OsSignals {
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}
#[cfg(unix)]
impl OsSignals {
    fn new() -> io::Result<Self> {
        Ok(Self {
            term: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            hup: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?,
        })
    }
    async fn recv(&mut self) -> Source {
        tokio::select! {Some(())=self.term.recv()=>Source::Term,Some(())=self.hup.recv()=>Source::Hup}
    }
}
#[cfg(not(unix))]
struct OsSignals;
#[cfg(not(unix))]
impl OsSignals {
    fn new() -> io::Result<Self> {
        Ok(Self)
    }
    async fn recv(&mut self) -> Source {
        std::future::pending().await
    }
}
/// One owner invocation; CLI subcommands do not install process signal handlers.
pub struct Termination {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}
impl Termination {
    pub fn install(runtime: &tokio::runtime::Handle) -> io::Result<Self> {
        let shared = Arc::new(Shared::new());
        let mut os = {
            let _entered = runtime.enter();
            OsSignals::new()?
        };
        #[cfg(windows)]
        windows::install(shared.clone())?;
        let observed = shared.clone();
        let task=runtime.spawn(async move {
            let mut deadline=None;
            loop {
                observed.flush();
                if observed.state.load(Ordering::Acquire)&3>=2 {observed.force(false).await;break;}
                if deadline.is_none() && observed.requested() {deadline=Some(tokio::time::Instant::now()+GRACE);}
                tokio::select! {biased;
                    _=observed.done.cancelled()=>break,
                    _=async {if let Some(at)=deadline {tokio::time::sleep_until(at).await}else{std::future::pending::<()>().await}}=>{observed.force(true).await;break;},
                    source=os.recv()=>observed.signal(source),
                    _=observed.notify.notified()=>{},
                }
            }
        });
        Ok(Self { shared, task })
    }
    pub fn requested(&self) -> bool {
        self.shared.requested()
    }
    pub fn forced(&self) -> bool {
        self.shared.forced.load(Ordering::Acquire)
    }
    pub fn terminal_lost(&self) {
        self.shared.lost();
    }
    pub fn bind_diagnostics(&self, diagnostics: Diagnostics) {
        let _ = self.shared.diagnostics.set(diagnostics);
        self.shared.notify.notify_one();
    }
    pub fn complete(&self) {
        self.shared.done.cancel();
        #[cfg(windows)]
        windows::complete();
    }
}
impl Drop for Termination {
    fn drop(&mut self) {
        self.complete();
        self.task.abort();
        #[cfg(windows)]
        windows::uninstall();
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::sync::atomic::AtomicPtr;
    use windows_sys::Win32::System::{
        Console::{
            CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
        },
        Threading::{CreateEventW, SetEvent, WaitForSingleObject},
    };
    static STATE: OnceLock<Arc<Shared>> = OnceLock::new();
    // One completion handle lives until process exit so a native callback can
    // never race handle destruction. The OS releases it when the owner exits.
    static COMPLETE: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
    unsafe extern "system" fn handler(event: u32) -> i32 {
        let source = match event {
            CTRL_CLOSE_EVENT => Source::ConsoleClose,
            CTRL_LOGOFF_EVENT => Source::ConsoleLogoff,
            CTRL_SHUTDOWN_EVENT => Source::ConsoleShutdown,
            _ => return 0,
        };
        let Some(state) = STATE.get() else {
            return 0;
        };
        state.signal(source);
        let event = COMPLETE.load(Ordering::Acquire);
        if !event.is_null() {
            unsafe {
                WaitForSingleObject(event, 4000);
            }
        }
        1
    }
    pub(super) fn install(state: Arc<Shared>) -> io::Result<()> {
        STATE.set(state).map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "owner console handler already installed",
            )
        })?;
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        COMPLETE.store(event, Ordering::Release);
        if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub(super) fn complete() {
        let event = COMPLETE.load(Ordering::Acquire);
        if !event.is_null() {
            unsafe {
                SetEvent(event);
            }
        }
    }
    pub(super) fn uninstall() {
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 0);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_loss_is_not_a_second_os_signal_and_notifications_are_bounded() {
        let state = Shared::new();
        state.lost();
        assert!(state.requested());
        state.signal(Source::Hup);
        assert_eq!(state.state.load(Ordering::Acquire) & 3, 1);
        state.signal(Source::Term);
        assert_eq!(state.state.load(Ordering::Acquire) & 3, 2);
        for _ in 0..100 {
            state.signal(Source::Hup);
        }
        assert_eq!(state.state.load(Ordering::Acquire) & 3, 2);
    }
}
