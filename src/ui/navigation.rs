//! Conversation navigation ownership. Structural migration: transitions retain their existing behaviour.
use super::*;

#[derive(Debug)]
pub struct Navigation {
    pub(super) committed_preview: Option<(String, String)>,
    pub anchor: ViewAnchor,
    mode: FollowMode,
    read: ReadState,
    pub(super) history_rebase_pending: bool,
    pub view_revision: u64,
    pub(super) scroll: VisualScroll,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            committed_preview: None,
            anchor: ViewAnchor::default(),
            mode: FollowMode::Following,
            read: ReadState::Idle,
            history_rebase_pending: false,
            view_revision: 0,
            scroll: VisualScroll::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct VisualScroll {
    pub(super) row: usize,
    pub(super) within: usize,
    pub(super) anchor: ViewAnchor,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) revision: u64,
    pub(super) pending: isize,
    pub(super) moved_during_read: bool,
    pub(super) focus: Option<String>,
    pub(super) initialized: bool,
    pub(super) origin: usize,
    pub(super) area: Option<Rect>,
}
impl Default for VisualScroll {
    fn default() -> Self {
        Self {
            row: 0,
            within: 0,
            anchor: ViewAnchor::default(),
            width: 78,
            height: 18,
            revision: 0,
            pending: 0,
            moved_during_read: false,
            focus: None,
            initialized: false,
            origin: 0,
            area: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FollowMode {
    Following,
    FollowingUnseen,
    Reading,
    ReadingUnseen,
}

type ReadTicket = (u64, Option<String>);

#[derive(Debug)]
enum ReadState {
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
}
impl Navigation {
    pub fn follow(&self) -> bool {
        matches!(
            self.mode,
            FollowMode::Following | FollowMode::FollowingUnseen
        )
    }
    pub fn unseen(&self) -> bool {
        matches!(
            self.mode,
            FollowMode::FollowingUnseen | FollowMode::ReadingUnseen
        )
    }
    pub fn set_follow(&mut self, follow: bool) {
        self.set_mode(follow, self.unseen());
    }
    pub fn set_unseen(&mut self, unseen: bool) {
        self.set_mode(self.follow(), unseen);
    }
    fn set_mode(&mut self, follow: bool, unseen: bool) {
        self.mode = match (follow, unseen) {
            (true, false) => FollowMode::Following,
            (true, true) => FollowMode::FollowingUnseen,
            (false, false) => FollowMode::Reading,
            (false, true) => FollowMode::ReadingUnseen,
        };
    }
    pub fn is_loading(&self) -> bool {
        matches!(self.read, ReadState::Hydrating | ReadState::Awaiting(_))
    }
    pub(super) fn request(&self) -> Option<&ReadTicket> {
        match &self.read {
            ReadState::Awaiting(ticket) | ReadState::Delivered(ticket) => Some(ticket),
            ReadState::Idle | ReadState::Hydrating => None,
        }
    }
    pub fn set_loading(&mut self, loading: bool) {
        let ticket = self.take_request();
        self.read = ReadState::new(loading, ticket);
    }
    pub(super) fn set_request(&mut self, ticket: Option<ReadTicket>) {
        self.read = ReadState::new(self.is_loading(), ticket);
    }
    pub(super) fn take_request(&mut self) -> Option<ReadTicket> {
        let empty = ReadState::new(self.is_loading(), None);
        match std::mem::replace(&mut self.read, empty) {
            ReadState::Awaiting(ticket) | ReadState::Delivered(ticket) => Some(ticket),
            ReadState::Idle | ReadState::Hydrating => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_mode_preserves_all_characterized_flag_combinations() {
        for follow in [false, true] {
            for unseen in [false, true] {
                let mut nav = Navigation::default();
                nav.set_follow(follow);
                nav.set_unseen(unseen);
                assert_eq!((nav.follow(), nav.unseen()), (follow, unseen));
                nav.set_follow(!follow);
                assert_eq!((nav.follow(), nav.unseen()), (!follow, unseen));
                nav.set_unseen(!unseen);
                assert_eq!((nav.follow(), nav.unseen()), (!follow, !unseen));
            }
        }
    }

    #[test]
    fn read_state_distinguishes_hydration_awaiting_and_delivered_tickets() {
        for loading in [false, true] {
            for owned in [false, true] {
                let mut nav = Navigation::default();
                let ticket = owned.then(|| (17, Some("node".into())));
                nav.set_loading(loading);
                nav.set_request(ticket.clone());
                assert_eq!(nav.is_loading(), loading);
                assert_eq!(nav.request(), ticket.as_ref());
                nav.set_loading(!loading);
                assert_eq!(nav.is_loading(), !loading);
                assert_eq!(nav.request(), ticket.as_ref());
                assert_eq!(nav.take_request(), ticket);
                assert_eq!(nav.is_loading(), !loading);
                assert!(nav.request().is_none());
            }
        }
    }
}
