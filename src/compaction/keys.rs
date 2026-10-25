//! Existing compaction queue/result/cache metadata namespaces.
pub(crate) struct CompactionKeys<'a>(&'a str);
impl<'a> CompactionKeys<'a> {
    pub(crate) fn new(thread: &'a str) -> Self {
        Self(thread)
    }
    pub(crate) fn pending(self) -> String {
        format!("compact.pending.{}", self.0)
    }
    pub(crate) fn inflight(self) -> String {
        format!("compact.inflight.{}", self.0)
    }
    pub(crate) fn result(operation: &str) -> String {
        format!("compact.result.{operation}")
    }
    pub(super) fn summary_part(digest: &str) -> String {
        format!("summary.part.{digest}")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_compaction_keys_are_byte_exact() {
        assert_eq!(
            CompactionKeys::new("thread:日本").pending().as_bytes(),
            b"compact.pending.thread:\xe6\x97\xa5\xe6\x9c\xac"
        );
        assert_eq!(
            CompactionKeys::new("thread:1").inflight(),
            "compact.inflight.thread:1"
        );
        assert_eq!(
            CompactionKeys::result("owner:operation"),
            "compact.result.owner:operation"
        );
        assert_eq!(CompactionKeys::summary_part("hash"), "summary.part.hash");
    }
}
