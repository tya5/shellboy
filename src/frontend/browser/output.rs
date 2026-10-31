//! Bounded post-commit hints for visible Watch shell ages, not a polling source.
use super::{BTreeMap, BTreeSet, LIMIT, PERIOD};
use std::time::Instant;
struct Shell {
    execution: String,
    observed: Instant,
    compared: Option<Instant>,
}
#[derive(Default)]
pub(super) struct Output {
    shells: BTreeMap<String, Shell>,
    hinted: bool,
    // Admission is shared across scope replacement and hiding.
    next: Option<tokio::time::Instant>,
}
impl Output {
    pub fn clear(&mut self) {
        self.shells.clear();
        self.hinted = false;
    }
    pub fn retain(&mut self, ids: &BTreeSet<String>) {
        self.shells.retain(|id, _| ids.contains(id));
        if self.shells.is_empty() {
            self.hinted = false;
        }
    }
    pub fn accept(&mut self, thread: String, shell: Option<(String, Instant)>) {
        self.shells.remove(&thread);
        if let Some((execution, observed)) =
            shell.filter(|(id, _)| !id.is_empty() && id.len() <= 256)
            && self.shells.len() < LIMIT
        {
            self.shells.insert(
                thread,
                Shell {
                    execution,
                    observed,
                    compared: None,
                },
            );
            self.hinted = true;
        }
    }
    pub fn notify(&mut self) {
        if !self.shells.is_empty() {
            self.hinted = true;
        }
    }
    pub fn deadline(&self) -> Option<tokio::time::Instant> {
        (self.hinted && !self.shells.is_empty())
            .then(|| self.next.unwrap_or_else(tokio::time::Instant::now))
    }
    pub fn take(&mut self, now: tokio::time::Instant) -> Option<Vec<String>> {
        if !self.hinted || self.shells.is_empty() || self.next.is_some_and(|at| at > now) {
            return None;
        }
        self.hinted = false;
        self.next = Some(now + PERIOD);
        Some(
            self.shells
                .values()
                .map(|s| s.execution.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        )
    }
    pub fn markers(&mut self, markers: Vec<(String, Option<Instant>)>) -> Vec<String> {
        let markers = markers.into_iter().take(LIMIT).collect::<BTreeMap<_, _>>();
        self.shells
            .iter_mut()
            .filter_map(|(thread, shell)| {
                let marker = markers.get(&shell.execution).copied().flatten()?;
                if marker <= shell.observed || shell.compared.is_some_and(|seen| seen >= marker) {
                    return None;
                }
                shell.compared = Some(marker);
                Some(thread.clone())
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_changed_visible_shell_marks_its_thread_and_old_or_missing_markers_do_not_poll() {
        let at = Instant::now();
        let now = tokio::time::Instant::now();
        let mut output = Output::default();
        output.accept("a".into(), Some(("shell-a".into(), at)));
        output.accept("b".into(), Some(("shell-b".into(), at)));
        assert_eq!(output.take(now).unwrap(), ["shell-a", "shell-b"]);
        assert!(
            output
                .markers(vec![("shell-a".into(), Some(at)), ("shell-b".into(), None)])
                .is_empty()
        );
        assert!(output.deadline().is_none());
        output.notify();
        assert!(
            output
                .take(now + PERIOD - Duration::from_millis(1))
                .is_none()
        );
        output.take(now + PERIOD).unwrap();
        let new = at + std::time::Duration::from_millis(10);
        assert_eq!(
            output.markers(vec![
                ("shell-a".into(), Some(new)),
                ("shell-b".into(), Some(at))
            ]),
            ["a"]
        );
        assert!(
            output
                .markers(vec![("shell-a".into(), Some(new))])
                .is_empty()
        );
        output.retain(&BTreeSet::from(["b".into()]));
        assert!(
            output
                .markers(vec![(
                    "shell-a".into(),
                    Some(new + std::time::Duration::from_secs(1))
                )])
                .is_empty()
        );
    }
    #[test]
    fn hiding_discards_all_hints_and_reopening_cannot_bypass_global_deadline() {
        let at = Instant::now();
        let now = tokio::time::Instant::now();
        let mut output = Output::default();
        output.accept("a".into(), Some(("x".into(), at)));
        output.take(now).unwrap();
        output.clear();
        for _ in 0..40 {
            output.notify();
            assert!(output.deadline().is_none());
            assert!(output.take(now + PERIOD).is_none());
        }
        output.accept("b".into(), Some(("y".into(), at)));
        assert!(
            output
                .take(now + PERIOD - Duration::from_millis(1))
                .is_none()
        );
        assert_eq!(output.take(now + PERIOD).unwrap(), ["y"]);
    }
    #[test]
    fn delayed_preparation_compares_against_db_read_time_and_new_read_consumes_old_marker() {
        let read = Instant::now();
        let commit = read + std::time::Duration::from_millis(1);
        let now = tokio::time::Instant::now();
        let mut output = Output::default();
        output.accept("a".into(), Some(("x".into(), read)));
        output.take(now).unwrap();
        assert_eq!(output.markers(vec![("x".into(), Some(commit))]), ["a"]);
        // A reply whose raw DB read preceded the commit still needs one follow-up.
        output.accept("a".into(), Some(("x".into(), read)));
        output.take(now + PERIOD).unwrap();
        assert_eq!(output.markers(vec![("x".into(), Some(commit))]), ["a"]);
        output.accept("a".into(), Some(("x".into(), commit)));
        output.take(now + 2 * PERIOD).unwrap();
        assert!(output.markers(vec![("x".into(), Some(commit))]).is_empty());
        assert!(output.deadline().is_none());
    }
    use std::time::Duration;
}
