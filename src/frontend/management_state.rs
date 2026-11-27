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
#[derive(Default)]
pub(super) struct Management {
    pub(super) request_id: u64,
    pub(super) detail: Option<Arc<Value>>,
    generation: u64,
    wait_cancel: CancellationToken,
    /// The command dispatched under `generation`, shown once its result is accepted.
    pending_shown: Option<(u64, Shown)>,
    shown: Option<Shown>,
}
impl Management {
    pub(super) fn dispatched(&mut self, generation: u64, shown: Option<Shown>) {
        self.pending_shown = shown.map(|shown| (generation, shown));
    }
    pub(super) fn shown(&self) -> Option<&Shown> {
        self.shown.as_ref()
    }
    pub(super) fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
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
    pub(super) fn begin_result(&mut self) -> u64 {
        self.request_id = self.request_id.wrapping_add(1);
        self.detail = None;
        self.shown = match self.pending_shown.take() {
            Some((generation, shown)) if generation == self.generation => Some(shown),
            _ => None,
        };
        self.request_id
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
