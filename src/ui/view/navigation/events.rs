//! Navigation events choose complete mode, read and projection transitions.
use super::*;

pub(in crate::ui) enum ResolutionCause {
    UserScroll { reached_tail: bool },
    ContentChange,
}

pub(in crate::ui) enum NavEvent {
    Cancel {
        keep_pending: bool,
    },
    StartRebase,
    TailRequested,
    Restored {
        anchor: ViewAnchor,
        follow: bool,
    },
    ReadingSelected,
    UnseenSelected(bool),
    Read(ReadEvent),
    ReplyConsumed,
    ReadRequested {
        ticket: ReadTicket,
        pending: Option<isize>,
        new_epoch: bool,
    },
    ScrollInitiated,
    FrameProjected {
        row: usize,
        width: u16,
        height: u16,
        revision: u64,
        origin: usize,
    },
    PositionResolved {
        position: Option<(ViewAnchor, usize)>,
        row: usize,
        cause: ResolutionCause,
        local_read: bool,
    },
    HistoryFocused {
        position: Option<(ViewAnchor, usize)>,
        row: usize,
        revision: u64,
        origin: usize,
    },
    ReasoningExpanded {
        anchor: ViewAnchor,
    },
    SourceInvalidated,
}
impl NavEvent {
    fn advances_epoch(&self) -> bool {
        matches!(
            self,
            Self::Cancel { .. }
                | Self::StartRebase
                | Self::TailRequested
                | Self::ReadRequested {
                    new_epoch: true,
                    ..
                }
                | Self::ScrollInitiated
                | Self::HistoryFocused { .. }
                | Self::ReasoningExpanded { .. }
        )
    }
}

impl Navigation {
    pub(in crate::ui) fn on_event(&mut self, event: NavEvent) -> read::ReadEffects {
        if event.advances_epoch() {
            self.view_revision = if matches!(&event, NavEvent::ReasoningExpanded { .. }) {
                self.view_revision + 1
            } else {
                self.view_revision.wrapping_add(1)
            };
        }
        let mut mode = None;
        let mut anchor = None;
        let mut within = None;
        let mut row = None;
        let mut geometry = None;
        let mut revision = None;
        let mut origin = None;
        let mut invalidate = false;
        let read = match event {
            NavEvent::Cancel { keep_pending } => Some(ReadEvent::Cancelled { keep_pending }),
            NavEvent::StartRebase => Some(ReadEvent::RebaseBoundary {
                ticket: (self.view_revision, None),
            }),
            NavEvent::TailRequested => {
                mode = Some(FollowMode::Following);
                Some(ReadEvent::Cancelled {
                    keep_pending: false,
                })
            }
            NavEvent::Restored {
                anchor: restored,
                follow,
            } => {
                anchor = Some(restored);
                mode = Some(FollowMode::new(follow, false));
                None
            }
            NavEvent::ReadingSelected => {
                mode = Some(FollowMode::new(false, self.unseen()));
                None
            }
            NavEvent::UnseenSelected(unseen) => {
                mode = Some(FollowMode::new(self.follow(), unseen));
                None
            }
            NavEvent::Read(event) => Some(event),
            NavEvent::ReplyConsumed => {
                invalidate = true;
                Some(ReadEvent::ReplyConsumed)
            }
            NavEvent::ReadRequested {
                ticket,
                pending,
                new_epoch,
            } => Some(ReadEvent::NavigationRequested {
                ticket: if new_epoch {
                    (self.view_revision, ticket.1)
                } else {
                    ticket
                },
                pending,
            }),
            NavEvent::ScrollInitiated => {
                mode = Some(FollowMode::new(false, self.unseen()));
                None
            }
            NavEvent::FrameProjected {
                row: projected,
                width,
                height,
                revision: source_revision,
                origin: source_origin,
            } => {
                row = Some(projected);
                geometry = Some((width, height));
                revision = Some(source_revision);
                origin = Some(source_origin);
                None
            }
            NavEvent::PositionResolved {
                position,
                row: projected,
                cause,
                local_read,
            } => {
                if let Some((source, offset)) = position {
                    anchor = Some(source);
                    within = Some(offset);
                }
                row = Some(projected);
                mode = match cause {
                    ResolutionCause::UserScroll { reached_tail: true } => {
                        Some(FollowMode::Following)
                    }
                    ResolutionCause::UserScroll {
                        reached_tail: false,
                    } => Some(FollowMode::new(false, self.unseen())),
                    ResolutionCause::ContentChange => None,
                };
                local_read.then_some(ReadEvent::PositionMoved)
            }
            NavEvent::HistoryFocused {
                position,
                row: projected,
                revision: source_revision,
                origin: source_origin,
            } => {
                if let Some((source, offset)) = position {
                    anchor = Some(source);
                    within = Some(offset);
                    row = Some(projected);
                    revision = Some(source_revision);
                    origin = Some(source_origin);
                }
                mode = Some(FollowMode::new(false, self.unseen()));
                Some(ReadEvent::Cancelled { keep_pending: true })
            }
            NavEvent::ReasoningExpanded { anchor: source } => {
                mode = Some(FollowMode::new(false, self.unseen()));
                anchor = Some(source);
                None
            }
            NavEvent::SourceInvalidated => {
                invalidate = true;
                None
            }
        };
        if let Some(mode) = mode {
            self.mode = mode;
        }
        if let Some(anchor) = anchor {
            self.anchor = anchor;
        }
        if let Some(row) = row {
            let previous = std::mem::take(&mut self.scroll);
            self.scroll = VisualScroll {
                position: ProjectionPosition::Current(row),
                within: within.unwrap_or(previous.within),
                anchor: self.anchor.clone(),
                width: geometry.map_or(previous.width, |g| g.0),
                height: geometry.map_or(previous.height, |g| g.1),
                revision: revision.unwrap_or(previous.revision),
                origin: origin.unwrap_or(previous.origin),
                ..previous
            };
        }
        if invalidate {
            self.scroll.position = ProjectionPosition::Stale(self.scroll.row());
        }
        read.map_or_else(read::ReadEffects::default, |event| {
            self.intent.on_event(event)
        })
    }
}
