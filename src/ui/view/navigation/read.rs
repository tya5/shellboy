//! Owned read lifecycle. Events replace the intent atomically; no I/O.
use super::ReadTicket;

#[derive(Debug, Default)]
pub(super) struct ReadIntent {
    state: ReadState,
    pending: isize,
    motion: ReadMotion,
}

#[derive(Debug, Default)]
enum ReadState {
    #[default]
    Idle,
    Hydrating,
    Awaiting(ReadTicket),
    Delivered(ReadTicket),
}
impl ReadState {
    fn new(loading: bool, ticket: Option<ReadTicket>) -> Self {
        match (loading, ticket) {
            (false, None) => Self::Idle,
            (true, None) => Self::Hydrating,
            (true, Some(ticket)) => Self::Awaiting(ticket),
            (false, Some(ticket)) => Self::Delivered(ticket),
        }
    }
    fn into_parts(self) -> (bool, Option<ReadTicket>) {
        match self {
            Self::Idle => (false, None),
            Self::Hydrating => (true, None),
            Self::Awaiting(ticket) => (true, Some(ticket)),
            Self::Delivered(ticket) => (false, Some(ticket)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum ReadMotion {
    #[default]
    Retained,
    Moved,
    Rebasing,
    RebasingMoved,
}
impl ReadMotion {
    fn moved(self) -> bool {
        matches!(self, Self::Moved | Self::RebasingMoved)
    }
    fn rebasing(self) -> bool {
        matches!(self, Self::Rebasing | Self::RebasingMoved)
    }
    fn with(self, moved: bool, rebasing: bool) -> Self {
        match (moved, rebasing) {
            (false, false) => Self::Retained,
            (true, false) => Self::Moved,
            (false, true) => Self::Rebasing,
            (true, true) => Self::RebasingMoved,
        }
    }
}

pub(in crate::ui) enum ReadEvent {
    NavigationRequested {
        ticket: ReadTicket,
        pending: Option<isize>,
    },
    RebaseRequested {
        ticket: ReadTicket,
        pending: Option<isize>,
    },
    Cancelled {
        keep_pending: bool,
    },
    Failed,
    ReplyConsumed,
    Deferred(isize),
    DeferredDiscarded,
    DeferredDrained,
    PositionMoved,
    PositionSettled,
    RebaseResolved,
    WaitStarted,
    DeliveryObserved,
    BodyHydrated,
    OwnerRejected,
    ThreadSelected,
    #[cfg(test)]
    SeedTicket(Option<ReadTicket>),
    #[cfg(test)]
    SeedPending(isize),
    #[cfg(test)]
    SeedRebasing(bool),
    #[cfg(test)]
    TakeOwned,
}

#[derive(Default)]
pub(in crate::ui) struct ReadEffects {
    pub pending: isize,
    #[cfg(test)]
    pub ticket: Option<ReadTicket>,
}
impl ReadIntent {
    pub(super) fn is_loading(&self) -> bool {
        matches!(self.state, ReadState::Hydrating | ReadState::Awaiting(_))
    }
    pub(super) fn request(&self) -> Option<&ReadTicket> {
        match &self.state {
            ReadState::Awaiting(ticket) | ReadState::Delivered(ticket) => Some(ticket),
            ReadState::Idle | ReadState::Hydrating => None,
        }
    }
    pub(super) fn pending(&self) -> isize {
        self.pending
    }
    pub(super) fn moved(&self) -> bool {
        self.motion.moved()
    }
    pub(super) fn rebasing(&self) -> bool {
        self.motion.rebasing()
    }

    pub(super) fn on_event(&mut self, event: ReadEvent) -> ReadEffects {
        let Self {
            state,
            pending,
            motion,
        } = std::mem::take(self);
        let (loading, ticket) = state.into_parts();
        #[cfg(test)]
        if matches!(&event, ReadEvent::TakeOwned) {
            *self = Self {
                state: ReadState::new(loading, None),
                pending,
                motion,
            };
            return ReadEffects { pending: 0, ticket };
        }
        let mut effects = ReadEffects::default();
        let (loading, ticket, pending, motion) = match event {
            ReadEvent::NavigationRequested {
                ticket,
                pending: delta,
            } => (true, Some(ticket), delta.unwrap_or(pending), motion),
            ReadEvent::RebaseRequested {
                ticket,
                pending: delta,
            } => (
                true,
                Some(ticket),
                delta.unwrap_or(pending),
                motion.with(motion.moved(), true),
            ),
            ReadEvent::Cancelled { keep_pending } => (
                loading && ticket.is_none(),
                None,
                if keep_pending { pending } else { 0 },
                ReadMotion::Retained,
            ),
            ReadEvent::Failed => (
                false,
                None,
                if ticket.is_some() { pending } else { 0 },
                ReadMotion::Retained,
            ),
            ReadEvent::ReplyConsumed | ReadEvent::DeferredDrained => {
                effects.pending = pending;
                if matches!(event, ReadEvent::ReplyConsumed) {
                    (loading, None, 0, motion.with(false, motion.rebasing()))
                } else {
                    (loading, ticket, 0, motion)
                }
            }
            ReadEvent::Deferred(delta) => (loading, ticket, pending.saturating_add(delta), motion),
            ReadEvent::DeferredDiscarded => (loading, ticket, 0, motion),
            ReadEvent::PositionMoved => (
                loading,
                ticket,
                pending,
                motion.with(true, motion.rebasing()),
            ),
            ReadEvent::PositionSettled => (
                loading,
                ticket,
                pending,
                motion.with(false, motion.rebasing()),
            ),
            ReadEvent::RebaseResolved => {
                (loading, ticket, pending, motion.with(motion.moved(), false))
            }
            ReadEvent::WaitStarted => (true, ticket, pending, motion),
            ReadEvent::DeliveryObserved => (false, ticket, pending, motion),
            ReadEvent::BodyHydrated => (ticket.is_some(), ticket, pending, motion),
            ReadEvent::OwnerRejected => {
                (false, None, pending, motion.with(false, motion.rebasing()))
            }
            ReadEvent::ThreadSelected => (true, None, 0, motion.with(false, motion.rebasing())),
            #[cfg(test)]
            ReadEvent::SeedTicket(ticket) => (loading, ticket, pending, motion),
            #[cfg(test)]
            ReadEvent::SeedPending(pending) => (loading, ticket, pending, motion),
            #[cfg(test)]
            ReadEvent::SeedRebasing(rebasing) => (
                loading,
                ticket,
                pending,
                motion.with(motion.moved(), rebasing),
            ),
            #[cfg(test)]
            ReadEvent::TakeOwned => unreachable!(),
        };
        *self = Self {
            state: ReadState::new(loading, ticket),
            pending,
            motion,
        };
        effects
    }
}
