//! One scoped output observer on the existing frontend loop/read workers.
use super::*;
use crate::ui::live_work::LiveOutputPreview;
pub(super) type ReadResult = Result<LiveOutputPreview, String>;

pub(super) struct Ticket {
    pub thread: String,
    pub generation: u64,
    pub revision: u64,
    pub execution: String,
    pub stop: CancellationToken,
}
struct Scope {
    thread: String,
    generation: u64,
    revision: u64,
    executions: Vec<String>,
    open: Option<String>,
    stop: CancellationToken,
}
#[derive(Default)]
pub(super) struct Observation {
    scope: Option<Scope>,
    seen: HashMap<String, std::time::Instant>,
    last: Option<(String, u64, u64)>,
    next: Option<tokio::time::Instant>,
    inflight: Option<(u64, String)>,
    dirty: bool,
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.clear();
    }
}
impl Observation {
    fn clear(&mut self) {
        if let Some(scope) = self.scope.take() {
            scope.stop.cancel();
        }
        self.seen.clear();
        self.inflight = None;
        self.dirty = false;
        // Keep global admission time across close/open and Thread switches.
    }
    pub fn retain(&mut self, thread: Option<&str>, generations: &HashMap<String, u64>) {
        if self.scope.as_ref().is_some_and(|s| {
            Some(s.thread.as_str()) != thread || generations.get(&s.thread) != Some(&s.generation)
        }) {
            self.clear();
        }
    }
    pub fn observe(
        &mut self,
        thread: String,
        generation: u64,
        revision: u64,
        executions: Vec<String>,
        open: Option<String>,
    ) -> Result<(), ApiError> {
        if executions.len() > 3
            || executions.iter().any(|id| id.is_empty() || id.len() > 256)
            || executions.iter().collect::<HashSet<_>>().len() != executions.len()
            || open.as_ref().is_some_and(|id| !executions.contains(id))
        {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Invalid visible live output scope",
            ));
        }
        if self
            .last
            .as_ref()
            .is_some_and(|(t, g, r)| t == &thread && *g == generation && *r >= revision)
        {
            return Ok(());
        }
        self.clear();
        self.last = Some((thread.clone(), generation, revision));
        if !executions.is_empty() {
            self.dirty = open.is_some();
            self.scope = Some(Scope {
                thread,
                generation,
                revision,
                executions,
                open,
                stop: CancellationToken::new(),
            });
        }
        Ok(())
    }
    /// Committed output entries for the visible executions of this Thread. Each
    /// entry's saved output time, read against the wall clock sampled with
    /// `instant`, gives the output age; the open execution's preview re-reads.
    pub fn output(
        &mut self,
        output: &[crate::protocol::EventOutput],
        instant: std::time::Instant,
        wall: chrono::DateTime<chrono::Utc>,
    ) -> Option<UiUpdate> {
        let scope = self.scope.as_ref()?;
        let mut observed = false;
        for entry in output.iter().filter(|entry| {
            entry.thread_id == scope.thread && scope.executions.contains(&entry.execution_id)
        }) {
            self.dirty |= scope.open.as_ref() == Some(&entry.execution_id);
            let Ok(saved) = chrono::DateTime::parse_from_rfc3339(&entry.output_at) else {
                continue;
            };
            // A saved time at or after the sample is no older than the sample.
            let age = wall
                .signed_duration_since(saved)
                .to_std()
                .unwrap_or_default();
            let Some(at) = instant.checked_sub(age) else {
                continue;
            };
            if self
                .seen
                .get(&entry.execution_id)
                .is_none_or(|old| *old < at)
            {
                self.seen.insert(entry.execution_id.clone(), at);
                observed = true;
            }
        }
        observed.then(|| UiUpdate::LiveWorkObserved {
            thread_id: scope.thread.clone(),
            generation: scope.generation,
            revision: scope.revision,
            // A complete bounded snapshot permits latest-value delivery without
            // losing an earlier observation for another visible execution.
            markers: scope
                .executions
                .iter()
                .filter_map(|id| self.seen.get(id).map(|at| (id.clone(), *at)))
                .collect(),
        })
    }
    /// Missed output entries: the open preview reloads the bounded retained tail.
    pub fn resync(&mut self) {
        self.dirty |= self
            .scope
            .as_ref()
            .is_some_and(|scope| scope.open.is_some());
    }
    pub fn deadline(&self) -> Option<tokio::time::Instant> {
        (self.dirty
            && self.inflight.is_none()
            && self.scope.as_ref().is_some_and(|s| s.open.is_some()))
        .then(|| self.next.unwrap_or_else(tokio::time::Instant::now))
    }
    pub fn take(&mut self, now: tokio::time::Instant) -> Option<Ticket> {
        // Test the saved deadline against the caller's instant. Recomputing
        // an immediate deadline with Instant::now here always moves it just
        // beyond `now` and can prevent the first read from ever starting.
        if !self.dirty
            || self.inflight.is_some()
            || self.scope.as_ref().is_none_or(|scope| scope.open.is_none())
            || self.next.is_some_and(|due| due > now)
        {
            return None;
        }
        let scope = self.scope.as_ref()?;
        let execution = scope.open.clone()?;
        self.inflight = Some((scope.revision, execution.clone()));
        self.dirty = false;
        self.next = Some(now + Duration::from_millis(500));
        Some(Ticket {
            thread: scope.thread.clone(),
            generation: scope.generation,
            revision: scope.revision,
            execution,
            stop: scope.stop.child_token(),
        })
    }
    pub fn complete(&mut self, ticket: &Ticket) -> bool {
        if !self.scope.as_ref().is_some_and(|s| {
            s.thread == ticket.thread
                && s.generation == ticket.generation
                && s.revision == ticket.revision
                && s.open.as_ref() == Some(&ticket.execution)
        }) {
            return false;
        }
        if self.inflight.as_ref() != Some(&(ticket.revision, ticket.execution.clone())) {
            return false;
        }
        self.inflight = None;
        true
    }
}

pub(super) async fn read(owner: &native_history::Owner<'_>, ticket: &Ticket) -> ReadResult {
    use crate::protocol::{
        ExecutionTarget, ShellDetails, ShellOutput, ShellReadArgs, ShellReadOwner, ShellState,
    };
    const LIMIT: usize = 4096;
    let execution = &ticket.execution;
    let shell: ShellDetails = owner
        .call(Command::ShellGet(ExecutionTarget {
            execution_id: execution.clone(),
        }))
        .await
        .map_err(|error| error.to_string())?;
    let run_id = shell
        .run_id
        .filter(|_| {
            shell.execution_id == *execution && shell.thread_id.as_deref() == Some(&ticket.thread)
        })
        .ok_or_else(|| "Live output owner changed".to_owned())?;
    let output: ShellOutput = owner
        .call(Command::ShellRead(ShellReadArgs {
            execution_id: execution.clone(),
            expected_owner: Some(ShellReadOwner {
                thread_id: ticket.thread.clone(),
                run_id,
            }),
            tail: true,
            segments: true,
            cursor: None,
            encoding: None,
            limit_bytes: LIMIT as u32,
        }))
        .await
        .map_err(|error| error.to_string())?;
    let terminal = matches!(
        shell.state,
        ShellState::Exited | ShellState::Failed | ShellState::Stopped
    );
    let page = super::streams::from_native_tail(execution, output, LIMIT, terminal)
        .map_err(|error| error.to_string())?;
    Ok(LiveOutputPreview {
        stdout: page.stdout,
        stderr: page.stderr,
        decode_uncertain: page.decode_uncertain || page.decode_errors,
        skipped: page.skipped,
        complete: page.terminal && page.output_complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const WALL: &str = "2026-11-27T00:00:10Z";
    fn wall() -> chrono::DateTime<chrono::Utc> {
        WALL.parse().unwrap()
    }
    fn entry(thread: &str, execution: &str) -> crate::protocol::EventOutput {
        entry_at(thread, execution, "2026-11-27T00:00:07Z")
    }
    fn entry_at(thread: &str, execution: &str, at: &str) -> crate::protocol::EventOutput {
        crate::protocol::EventOutput {
            thread_id: thread.into(),
            execution_id: execution.into(),
            output_at: at.into(),
            seq: 1,
        }
    }
    #[test]
    fn output_age_comes_from_the_saved_time_and_snapshots_keep_other_executions() {
        let mut view = Observation::default();
        view.observe("t".into(), 1, 1, vec!["a".into(), "b".into()], None)
            .unwrap();
        let sample = std::time::Instant::now();
        view.output(&[entry("t", "a")], sample, wall()).unwrap();
        let Some(UiUpdate::LiveWorkObserved { markers, .. }) = view.output(
            &[entry_at("t", "b", "2026-11-27T00:00:09Z")],
            sample,
            wall(),
        ) else {
            panic!()
        };
        let three = std::time::Duration::from_secs(3);
        let one = std::time::Duration::from_secs(1);
        assert_eq!(
            markers,
            vec![("a".into(), sample - three), ("b".into(), sample - one)]
        );
        assert!(
            view.output(&[entry("t", "a")], sample, wall()).is_none(),
            "an output time already shown changes nothing"
        );
        assert!(
            view.output(&[entry("other", "a")], sample, wall())
                .is_none()
        );
        assert!(
            view.output(&[entry_at("t", "a", "not a time")], sample, wall())
                .is_none()
        );
        let Some(UiUpdate::LiveWorkObserved { markers, .. }) = view.output(
            &[entry_at("t", "a", "2026-11-27T00:00:11Z")],
            sample,
            wall(),
        ) else {
            panic!()
        };
        assert_eq!(
            markers[0],
            ("a".into(), sample),
            "a later saved time is not older than now"
        );
    }
    #[test]
    fn scope_rate_limit_markers_and_cancellation_do_not_leak_into_another_execution() {
        let mut view = Observation::default();
        let now = tokio::time::Instant::now();
        view.observe("t".into(), 1, 1, vec!["e".into()], None)
            .unwrap();
        assert!(view.deadline().is_none());
        assert!(
            view.output(&[entry("t", "foreign")], std::time::Instant::now(), wall())
                .is_none()
        );
        view.observe("t".into(), 1, 2, vec!["e".into()], Some("e".into()))
            .unwrap();
        let ticket = view.take(now).unwrap();
        assert!(view.take(now).is_none());
        view.observe("t".into(), 1, 3, vec!["next".into()], Some("next".into()))
            .unwrap();
        assert!(ticket.stop.is_cancelled() && !view.complete(&ticket));
        assert!(view.take(now + Duration::from_millis(499)).is_none());
        let next = view.take(now + Duration::from_millis(500)).unwrap();
        let at = std::time::Instant::now();
        assert!(view.output(&[entry("t", "next")], at, wall()).is_some());
        assert!(view.complete(&next));
        assert!(view.take(now + Duration::from_millis(999)).is_none());
        assert!(view.take(now + Duration::from_millis(1000)).is_some());
        view.observe("t".into(), 1, 4, vec![], None).unwrap();
        assert!(view.deadline().is_none());
        assert!(view.output(&[entry("t", "next")], at, wall()).is_none());
    }
}
