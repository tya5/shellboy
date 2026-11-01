//! Conversation navigation ownership. Structural migration: transitions retain their existing behaviour.
use super::*;

#[derive(Debug)]
pub struct Navigation {
    pub(super) committed_preview: Option<(String, String)>,
    pub anchor: ViewAnchor,
    pub follow: bool,
    pub unseen: bool,
    pub loading: bool,
    pub(super) history_rebase_pending: bool,
    pub view_revision: u64,
    pub(super) scroll: VisualScroll,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            committed_preview: None,
            anchor: ViewAnchor::default(),
            follow: true,
            unseen: false,
            loading: false,
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
    pub(super) request: Option<(u64, Option<String>)>,
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
            request: None,
            moved_during_read: false,
            focus: None,
            initialized: false,
            origin: 0,
            area: None,
        }
    }
}
