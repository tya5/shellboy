//! The shell each visible Watch detail shows, matched against committed output entries.
use super::{BTreeMap, BTreeSet, LIMIT};
#[derive(Default)]
pub(super) struct Output {
    /// Thread -> the execution its accepted detail shows.
    shells: BTreeMap<String, String>,
}
impl Output {
    pub fn clear(&mut self) {
        self.shells.clear();
    }
    pub fn retain(&mut self, ids: &BTreeSet<String>) {
        self.shells.retain(|id, _| ids.contains(id));
    }
    pub fn accept(&mut self, thread: String, execution: Option<String>) {
        self.shells.remove(&thread);
        if let Some(execution) = execution.filter(|id| !id.is_empty() && id.len() <= 256)
            && self.shells.len() < LIMIT
        {
            self.shells.insert(thread, execution);
        }
    }
    /// Threads whose shown execution committed output; each re-reads its detail.
    pub fn changed(&self, output: &[crate::protocol::EventOutput]) -> Vec<String> {
        self.shells
            .iter()
            .filter(|(_, execution)| output.iter().any(|entry| &entry.execution_id == *execution))
            .map(|(thread, _)| thread.clone())
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(execution: &str) -> crate::protocol::EventOutput {
        crate::protocol::EventOutput {
            thread_id: "owner".into(),
            execution_id: execution.into(),
            seq: 1,
        }
    }
    #[test]
    fn only_a_shown_shell_marks_its_thread() {
        let mut shown = Output::default();
        shown.accept("a".into(), Some("shell-a".into()));
        shown.accept("b".into(), Some("shell-b".into()));
        shown.accept("c".into(), None);
        assert_eq!(shown.changed(&[entry("shell-a")]), ["a"]);
        assert!(shown.changed(&[]).is_empty());
        assert!(shown.changed(&[entry("shell-z")]).is_empty());
        shown.retain(&BTreeSet::from(["b".into()]));
        assert!(shown.changed(&[entry("shell-a")]).is_empty());
        assert_eq!(shown.changed(&[entry("shell-a"), entry("shell-b")]), ["b"]);
        shown.clear();
        assert!(shown.changed(&[entry("shell-b")]).is_empty());
    }
}
