//! Result generation and wait-observation ownership, independent of draft images.
use crate::protocol::Command;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
#[derive(Default)]
pub(super) struct Management {
    pub(super) inspection: super::inspection_state::Observation,
    pub(super) request_id: u64,
    pub(super) detail: Option<Arc<Value>>,
    pub(super) current_history_detail: Option<Arc<Value>>,
    generation: u64,
    wait_cancel: CancellationToken,
}
impl Management {
    pub(super) fn next_generation(&mut self) -> u64 {
        self.inspection.close();
        self.current_history_detail = None;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
    pub(super) fn accepts(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub(super) fn begin_retained_update(&mut self) -> u64 {
        self.request_id = self.request_id.wrapping_add(1);
        self.request_id
    }
    pub(super) fn detail_source(&self, pointer: &str) -> Option<(Arc<Value>, String)> {
        if (pointer == "/current_source" || pointer.starts_with("/current_source/"))
            && let Some(value) = &self.current_history_detail
        {
            return Some((Arc::clone(value), pointer.into()));
        }
        self.detail
            .as_ref()
            .map(|value| (Arc::clone(value), pointer.into()))
    }
    pub(super) fn begin_result(&mut self) -> u64 {
        self.request_id = self.request_id.wrapping_add(1);
        self.detail = None;
        self.current_history_detail = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_updates_preserve_original_arc_and_resolve_only_last_presented_metadata() {
        let mut manager = Management::default();
        let original = Arc::new(
            serde_json::json!({"text":"retained original","current_source_suffix":"original field"}),
        );
        let presented = Arc::new(
            serde_json::json!({"current_source":{"nodes":[{"node_id":"n","body_available":false}]}}),
        );
        manager.detail = Some(Arc::clone(&original));
        manager.current_history_detail = Some(Arc::clone(&presented));
        assert_eq!(manager.begin_retained_update(), 1);
        let (value, pointer) = manager.detail_source("/text").unwrap();
        assert!(Arc::ptr_eq(&value, &original));
        assert_eq!(pointer, "/text");
        let (value, pointer) = manager
            .detail_source("/current_source/nodes/0/body_available")
            .unwrap();
        assert!(Arc::ptr_eq(&value, &presented));
        assert_eq!(value.pointer(&pointer), Some(&Value::Bool(false)));
        let (value, _) = manager.detail_source("/current_source_suffix").unwrap();
        assert!(Arc::ptr_eq(&value, &original));
        manager.begin_result();
        assert!(manager.detail.is_none() && manager.current_history_detail.is_none());
    }
}
