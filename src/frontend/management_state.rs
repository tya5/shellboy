//! Result generation and wait-observation ownership, independent of draft images.
use crate::protocol::Command;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
#[derive(Default)]
pub(super) struct Management {
    pub(super) request_id: u64,
    pub(super) detail: Option<Arc<Value>>,
    generation: u64,
    wait_cancel: CancellationToken,
}
impl Management {
    pub(super) fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
    pub(super) fn accepts(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub(super) fn begin_result(&mut self) -> u64 {
        self.request_id = self.request_id.wrapping_add(1);
        self.detail = None;
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
