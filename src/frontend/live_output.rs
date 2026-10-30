//! One scoped output observer on the existing frontend loop/read workers.
use super::*;
use crate::control::output::preview::live::LiveCursor;
use crate::ui::live_work::LiveOutputPreview;
pub(super) type ReadResult = Result<(LiveCursor, LiveOutputPreview), String>;

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
    cursor: Option<LiveCursor>,
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
        self.cursor = None;
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
    pub fn executions(&self) -> Option<Vec<String>> {
        self.scope.as_ref().map(|s| s.executions.clone())
    }
    pub fn markers(&mut self, markers: Vec<(String, std::time::Instant)>) -> Option<UiUpdate> {
        let scope = self.scope.as_ref()?;
        let markers = markers
            .into_iter()
            .filter(|(id, at)| {
                scope.executions.contains(id) && self.seen.get(id).is_none_or(|old| old < at)
            })
            .collect::<Vec<_>>();
        if markers.is_empty() {
            return None;
        }
        for (id, at) in &markers {
            self.seen.insert(id.clone(), *at);
            self.dirty |= scope.open.as_ref() == Some(id);
        }
        Some(UiUpdate::LiveWorkObserved {
            thread_id: scope.thread.clone(),
            generation: scope.generation,
            revision: scope.revision,
            markers,
        })
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
    pub fn take_cursor(&mut self) -> Option<LiveCursor> {
        self.cursor.take()
    }
    pub fn install_cursor(&mut self, cursor: LiveCursor) {
        self.cursor = Some(cursor);
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

pub(super) async fn read(
    control: &Control,
    execution: &str,
    cursor: Option<LiveCursor>,
) -> ReadResult {
    let (cursor, page) =
        super::streams::live_execution_preview(control, execution.to_owned(), cursor)
            .await
            .map_err(|e| e.to_string())?;
    if page.raw_bytes > 4096
        || page.stdout.len() + page.stderr.len() > 4096
        || page.inspected_rows > 128
    {
        return Err("Live output exceeded its finite read allowance".into());
    }
    Ok((
        cursor,
        LiveOutputPreview {
            stdout: page.stdout,
            stderr: page.stderr,
            decode_uncertain: page.decode_uncertain || page.decode_errors,
            skipped: page.skipped,
            complete: page.terminal && page.output_complete,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_rate_limit_markers_and_cancellation_do_not_leak_into_another_execution() {
        let mut view = Observation::default();
        let now = tokio::time::Instant::now();
        view.observe("t".into(), 1, 1, vec!["e".into()], None)
            .unwrap();
        assert!(view.deadline().is_none());
        assert!(
            view.markers(vec![("foreign".into(), std::time::Instant::now())])
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
        assert!(view.markers(vec![("next".into(), at)]).is_some());
        assert!(view.markers(vec![("next".into(), at)]).is_none());
        assert!(view.complete(&next));
        assert!(view.take(now + Duration::from_millis(999)).is_none());
        assert!(view.take(now + Duration::from_millis(1000)).is_some());
        view.observe("t".into(), 1, 4, vec![], None).unwrap();
        assert!(view.deadline().is_none() && view.executions().is_none());
    }
}
