//! Test-only observation at actual preparation/admission sites. No product state.
use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) static FRONTEND_RELOADS: AtomicU64 = AtomicU64::new(0);
pub(crate) static FRONTEND_ACTIONS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SNAPSHOTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static CLONED_BYTES: AtomicU64 = AtomicU64::new(0);
pub(crate) static GATE_READS: AtomicU64 = AtomicU64::new(0);
pub(crate) static GATE_NANOS: AtomicU64 = AtomicU64::new(0);
pub(crate) fn snapshot(bytes: usize) {
    SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
    CLONED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}
pub(crate) fn reset() {
    for counter in [
        &SNAPSHOTS,
        &CLONED_BYTES,
        &GATE_READS,
        &GATE_NANOS,
        &FRONTEND_RELOADS,
        &FRONTEND_ACTIONS,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
}
pub(crate) fn seal(node: &mut crate::ui::UiNode) {
    super::streams::seal_node(node);
}
