//! Result generation and wait-observation ownership, independent of draft images.
use crate::protocol::{Command, EventsWaitResult};
use crate::ui::ManagementContext;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
/// A shown result that the saved-change wake can read again.
#[derive(Clone)]
pub(super) struct Shown {
    pub(super) command: Command,
    pub(super) title: String,
    pub(super) context: Option<ManagementContext>,
    pub(super) work_status: bool,
}
impl Shown {
    /// The commands whose results the wake's marks describe: `config.get`,
    /// `index.status` and `storage.status`.
    pub(super) fn of(
        command: &Command,
        title: &str,
        context: &Option<ManagementContext>,
        work_status: bool,
    ) -> Option<Self> {
        matches!(
            command,
            Command::ConfigGet(_) | Command::IndexStatus(_) | Command::StorageStatus(_)
        )
        .then(|| Self {
            command: command.clone(),
            title: title.to_owned(),
            context: context.clone(),
            work_status,
        })
    }
    /// Whether this wake page reports a change to what the command reads.
    pub(super) fn is_stale_after(&self, page: &EventsWaitResult, resync: bool) -> bool {
        resync
            || match self.command {
                Command::ConfigGet(_) => page.config_changed,
                Command::IndexStatus(_) => page.index_changed,
                _ => page.retention_changed,
            }
    }
}
/// What the one management command slot of the client is waiting for.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Flight {
    #[default]
    Idle,
    /// A command the user ran; its result is shown as a new view.
    User,
    /// A read of the shown result; its result replaces the fields in place.
    Refresh,
}
/// How an accepted result is applied.
pub(super) enum Arrival {
    /// A new view with this request id.
    Replace(u64),
    /// The shown result read again: update it where it stands.
    Refresh,
}
/// One owner for shown and in-flight results. Each result carries its own mark.
struct Marked {
    shown: Shown,
    dirty: bool,
}
#[derive(Default)]
pub(super) struct Management {
    pub(super) request_id: u64,
    pub(super) detail: Option<Arc<Value>>,
    generation: u64,
    wait_cancel: CancellationToken,
    flight: Flight,
    /// The command dispatched under `generation`, shown once its result is accepted.
    pending_shown: Option<(u64, Marked)>,
    shown: Option<Marked>,
}
impl Management {
    pub(super) fn dispatched(&mut self, generation: u64, shown: Option<Shown>) {
        self.pending_shown = shown.map(|shown| {
            (
                generation,
                Marked {
                    shown,
                    dirty: false,
                },
            )
        });
    }
    pub(super) fn mark(&mut self, page: &EventsWaitResult, resync: bool) {
        for result in self
            .shown
            .iter_mut()
            .chain(self.pending_shown.iter_mut().map(|(_, result)| result))
        {
            result.dirty |= result.shown.is_stale_after(page, resync);
        }
    }
    pub(super) fn refresh_ready(&self) -> bool {
        self.flight == Flight::Idle && self.shown.as_ref().is_some_and(|result| result.dirty)
    }
    pub(super) fn start_refresh(&mut self) -> Option<(u64, Shown)> {
        if !self.refresh_ready() {
            return None;
        }
        let result = self.shown.as_mut()?;
        let shown = result.shown.clone();
        result.dirty = false;
        self.generation = self.generation.wrapping_add(1);
        self.flight = Flight::Refresh;
        Some((self.generation, shown))
    }
    /// F8 closed: no refresh is read for it any more.
    pub(super) fn close(&mut self) {
        self.shown = None;
        self.pending_shown = None;
        if self.flight == Flight::Refresh {
            self.generation = self.generation.wrapping_add(1);
            self.flight = Flight::Idle;
        }
    }
    pub(super) fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.flight = Flight::User;
        self.pending_shown = None;
        self.generation
    }
    pub(super) fn accepts(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub(super) fn detail_source(&self, pointer: &str) -> Option<(Arc<Value>, String)> {
        self.detail
            .as_ref()
            .map(|value| (Arc::clone(value), pointer.into()))
    }
    /// Apply an accepted result. A user result starts a new view and the shown
    /// command it carries; pending is left for the slot-free check.
    pub(super) fn begin_result(&mut self) -> Arrival {
        if std::mem::take(&mut self.flight) == Flight::Refresh {
            return Arrival::Refresh;
        }
        self.request_id = self.request_id.wrapping_add(1);
        self.detail = None;
        self.shown = match self.pending_shown.take() {
            Some((generation, shown)) if generation == self.generation => Some(shown),
            _ => None,
        };
        Arrival::Replace(self.request_id)
    }
    pub(super) fn observe(&self, command: &Command) -> CancellationToken {
        if matches!(command, Command::Wait(_)) {
            self.wait_cancel.child_token()
        } else {
            CancellationToken::new()
        }
    }
    pub(super) fn cancel_wait(&mut self) {
        self.wait_cancel.cancel();
        self.wait_cancel = CancellationToken::new();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Command, ConfigGetArgs, IndexStatusArgs};

    fn index_changed() -> EventsWaitResult {
        serde_json::from_value(serde_json::json!({
            "status": "changed", "cursor": "c", "changes": [], "more": false,
            "config_changed": false, "index_changed": true, "retention_changed": false,
        }))
        .unwrap()
    }
    /// Run `command` as the user and accept its result, so it is the shown one.
    fn show(management: &mut Management, command: Command) {
        let generation = management.next_generation();
        management.dispatched(generation, Shown::of(&command, "t", &None, false));
        assert!(management.accepts(generation));
        assert!(matches!(management.begin_result(), Arrival::Replace(_)));
    }
    fn index() -> Command {
        Command::IndexStatus(IndexStatusArgs::default())
    }

    #[test]
    fn user_command_result_survives_a_mark_that_arrives_before_it() {
        let mut management = Management::default();
        show(&mut management, index());
        let generation = management.next_generation();
        management.dispatched(
            generation,
            Shown::of(&Command::ConfigGet(ConfigGetArgs {}), "c", &None, false),
        );
        management.mark(&index_changed(), false);
        assert!(
            management.accepts(generation),
            "the mark did not supersede the command the user ran"
        );
        assert!(matches!(management.begin_result(), Arrival::Replace(_)));
        assert!(
            management.start_refresh().is_none(),
            "Index mark must not reread Config"
        );
    }

    #[test]
    fn deferred_mark_after_an_older_read_is_not_lost() {
        let mut management = Management::default();
        show(&mut management, index());
        management.mark(&index_changed(), false);
        let (generation, _) = management.start_refresh().unwrap();
        // A second mark finds the first read in flight and is kept as pending.
        management.mark(&index_changed(), false);
        assert!(management.accepts(generation));
        assert!(matches!(management.begin_result(), Arrival::Refresh));
        assert!(
            management.refresh_ready(),
            "the older read's result did not consume it"
        );
        assert!(management.start_refresh().is_some());
        assert!(!management.refresh_ready());
    }

    #[test]
    fn first_read_keeps_a_mark_received_before_its_result() {
        let mut management = Management::default();
        let generation = management.next_generation();
        management.dispatched(generation, Shown::of(&index(), "t", &None, false));
        management.mark(&index_changed(), false);
        assert!(!management.refresh_ready());
        assert!(matches!(management.begin_result(), Arrival::Replace(_)));
        assert!(management.refresh_ready());
    }

    #[test]
    fn closing_f8_stops_refreshes() {
        let mut management = Management::default();
        show(&mut management, index());
        management.mark(&index_changed(), false);
        let (generation, _) = management.start_refresh().unwrap();
        management.mark(&index_changed(), false);
        management.close();
        assert!(
            !management.accepts(generation),
            "the refresh in flight is dropped"
        );
        assert!(!management.refresh_ready());
        management.mark(&index_changed(), false);
        assert!(management.start_refresh().is_none());
    }
}
