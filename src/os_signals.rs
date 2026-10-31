//! OS signal reception, independent of terminal UI policy.
use std::io;

pub(crate) enum Signal {
    Term,
    Hup,
}

pub(crate) struct OsSignals {
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}
impl OsSignals {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            term: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            hup: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?,
        })
    }
    pub(crate) async fn recv(&mut self) -> Signal {
        tokio::select! {
            Some(()) = self.term.recv() => Signal::Term,
            Some(()) = self.hup.recv() => Signal::Hup,
            else => std::future::pending().await,
        }
    }
}

/// Serve observes Ctrl+C without changing TUI's keyboard/signal policy.
pub(crate) struct ServeSignals {
    common: OsSignals,
    interrupt: tokio::signal::unix::Signal,
}
impl ServeSignals {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            common: OsSignals::new()?,
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        })
    }
    pub(crate) async fn recv(&mut self) {
        tokio::select! {
            _ = self.common.recv() => {},
            Some(()) = self.interrupt.recv() => {},
            else => std::future::pending::<()>().await,
        }
    }
}
