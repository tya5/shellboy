//! One explicit RAG invocation's provider budget, never persistent index state.
#[derive(Default)]
pub(super) struct ProviderBreaker {
    failures: usize,
    paused: bool,
}
impl ProviderBreaker {
    pub(super) fn paused(&self) -> bool {
        self.paused
    }
    pub(super) fn failure(&mut self, code: &str) {
        if matches!(code, "embedding_failed" | "embedding_paused") {
            self.failures += 1;
        }
        self.paused |= code == "embedding_paused" || self.failures >= 3;
    }
    pub(super) fn success(&mut self) {
        self.failures = 0;
    }
}
