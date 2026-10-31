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
        tokio::select! {Some(())=self.term.recv()=>Signal::Term,Some(())=self.hup.recv()=>Signal::Hup}
    }
}
