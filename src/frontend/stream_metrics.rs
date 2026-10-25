//! Test-only observation at actual preparation/admission sites. No product state.
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
pub(crate) static SNAPSHOTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static CLONED_BYTES: AtomicU64 = AtomicU64::new(0);
pub(crate) static GATE_READS: AtomicU64 = AtomicU64::new(0);
pub(crate) static GATE_NANOS: AtomicU64 = AtomicU64::new(0);
pub(crate) fn snapshot(bytes: usize) {
    SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
    CLONED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}
pub(crate) fn reset() {
    for counter in [&SNAPSHOTS, &CLONED_BYTES, &GATE_READS, &GATE_NANOS] {
        counter.store(0, Ordering::Relaxed);
    }
}
pub(crate) struct Gate(Instant);
impl Gate {
    pub(crate) fn start() -> Self {
        GATE_READS.fetch_add(1, Ordering::Relaxed);
        Self(Instant::now())
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        GATE_NANOS.fetch_add(self.0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}
pub(crate) fn seal(node: &mut crate::ui::UiNode) {
    super::streams::seal_node(node);
}
