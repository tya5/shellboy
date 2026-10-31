//! One bounded visible-browser observer on existing frontend read admission.
use super::*;
use crate::ui::watch::BrowserMode;
use std::collections::{BTreeMap, BTreeSet};

mod output;
const LIMIT: usize = 128;
const PERIOD: Duration = Duration::from_millis(500);
struct Scope {
    generation: u64,
    filter: String,
    selected: Option<String>,
    visible: bool,
    mode: BrowserMode,
    descendant_root: Option<String>,
    stop: CancellationToken,
    detail_ids: BTreeSet<String>,
    detail_parents: BTreeSet<String>,
    detail_stop: CancellationToken,
    known: BTreeMap<String, u64>,
    cached_scope: Option<(bool, Vec<String>)>,
    scope_sequence: u64,
}
#[derive(Clone)]
pub(super) struct Ticket {
    pub generation: u64,
    pub ids: Vec<String>,
    pub selected: Option<String>,
    pub stop: CancellationToken,
    pub mode: BrowserMode,
    pub descendant_root: Option<String>,
    pub cached_scope: Option<(bool, Vec<String>)>,
    pub detail_ids: Vec<String>,
    pub detail_parent_ids: Vec<String>,
    pub detail_stop: CancellationToken,
    pub known_requested_ids: Vec<String>,
    serial: u64,
}
#[derive(Default)]
pub(super) struct Browser {
    output: output::Output,
    scope: Option<Scope>,
    last_generation: Option<u64>,
    last_visible: bool,
    dirty: BTreeSet<String>,
    detail_dirty: BTreeSet<String>,
    inflight: Option<(u64, Vec<String>, Vec<String>)>,
    serial: u64,
    next: Option<tokio::time::Instant>,
    resync: Option<(u64, String)>,
}
impl Drop for Browser {
    fn drop(&mut self) {
        self.close();
    }
}
impl Browser {
    pub fn output_notified(&mut self) {
        if self
            .scope
            .as_ref()
            .is_some_and(|s| s.visible && s.mode == BrowserMode::Watch)
            && self.resync.is_none()
        {
            self.output.notify();
        }
    }
    pub fn output_deadline(&self) -> Option<tokio::time::Instant> {
        self.output.deadline()
    }
    pub fn take_output_ids(&mut self, now: tokio::time::Instant) -> Option<Vec<String>> {
        self.output.take(now)
    }
    pub fn output_markers(&mut self, markers: Vec<(String, Option<std::time::Instant>)>) {
        if self
            .scope
            .as_ref()
            .is_some_and(|s| s.visible && s.mode == BrowserMode::Watch)
            && self.resync.is_none()
        {
            self.detail_dirty.extend(self.output.markers(markers));
        }
    }
    pub fn accept_details(
        &mut self,
        ticket: &Ticket,
        details: &[crate::ui::watch::PreparedWatchDetail],
    ) {
        if !self.details_current(ticket) || ticket.mode != BrowserMode::Watch {
            return;
        }
        let Some(scope) = self.scope.as_ref() else {
            return;
        };
        for id in ticket
            .detail_ids
            .iter()
            .filter(|id| scope.detail_ids.contains(*id))
        {
            self.output.accept(id.clone(), None);
        }
        for detail in details.iter().take(LIMIT) {
            let facts = detail.facts();
            if !scope.detail_ids.contains(&facts.thread_id)
                || !ticket.detail_ids.contains(&facts.thread_id)
            {
                continue;
            }
            let shell = facts.work.as_ref().and_then(|work| {
                work.rows.iter().find_map(|row| match &row.target {
                    crate::ui::live_work::LiveWorkTarget::Shell { execution_id, .. } => {
                        Some((execution_id.clone(), work.observed_at))
                    }
                    _ => None,
                })
            });
            self.output.accept(facts.thread_id.clone(), shell);
        }
    }
    pub fn generation(&self) -> Option<u64> {
        self.scope.as_ref().map(|scope| scope.generation)
    }
    fn close(&mut self) {
        self.output.clear();
        if let Some(s) = self.scope.take() {
            s.stop.cancel();
            s.detail_stop.cancel();
        }
        self.dirty.clear();
        self.detail_dirty.clear();
        self.inflight = None;
    }
    pub fn observe(
        &mut self,
        generation: u64,
        filter: String,
        selected: Option<String>,
        visible: bool,
    ) -> Result<(), ApiError> {
        self.observe_view(
            generation,
            filter,
            selected,
            visible,
            BrowserMode::List,
            None,
            Vec::new(),
            Vec::new(),
        )
    }
    // One existing UI observation packet is validated atomically, including
    // its mode, root and visible detail dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_view(
        &mut self,
        generation: u64,
        filter: String,
        selected: Option<String>,
        visible: bool,
        mode: BrowserMode,
        descendant_root: Option<String>,
        detail_ids: Vec<String>,
        detail_parent_ids: Vec<String>,
    ) -> Result<(), ApiError> {
        if self.last_generation.is_some_and(|last| generation < last) {
            return Ok(());
        }
        let valid = |id: &str| !id.is_empty() && id.len() <= 256;
        if filter.chars().count() > 256
            || selected.as_ref().is_some_and(|id| !valid(id))
            || descendant_root.as_ref().is_some_and(|id| !valid(id))
            || detail_ids.len() > LIMIT
            || detail_parent_ids.len() > LIMIT
            || detail_ids
                .iter()
                .chain(&detail_parent_ids)
                .any(|id| !valid(id))
        {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Invalid browser observation scope",
            ));
        }
        self.last_generation = Some(generation);
        self.last_visible = visible;
        if !visible {
            self.output.clear();
            if let Some(s) = &mut self.scope {
                s.visible = false;
                s.stop.cancel();
                s.detail_stop.cancel();
            }
            if let Some((_, ids, details)) = self.inflight.take() {
                for id in ids {
                    self.changed(&id);
                }
                self.detail_dirty.extend(details);
            }
            return Ok(());
        }
        let replace = self.scope.as_ref().is_none_or(|s| {
            s.generation != generation
                || s.filter != filter
                || s.mode != mode
                || s.descendant_root != descendant_root
        });
        if replace {
            self.close();
            self.resync = None;
            self.scope = Some(Scope {
                generation,
                filter,
                selected: None,
                visible: true,
                mode,
                descendant_root,
                stop: CancellationToken::new(),
                detail_ids: BTreeSet::new(),
                detail_parents: BTreeSet::new(),
                detail_stop: CancellationToken::new(),
                known: BTreeMap::new(),
                cached_scope: None,
                scope_sequence: 0,
            });
        }
        let s = self.scope.as_mut().expect("visible browser owns scope");
        if !s.visible {
            s.stop = CancellationToken::new();
            s.detail_stop = CancellationToken::new();
            s.visible = true;
            self.detail_dirty.extend(s.detail_ids.iter().cloned());
        }
        let detail_ids = detail_ids.into_iter().collect::<BTreeSet<_>>();
        if mode == BrowserMode::Watch {
            self.output.retain(&detail_ids);
        } else {
            self.output.clear();
        }
        let parents = detail_parent_ids.into_iter().collect::<BTreeSet<_>>();
        if s.detail_ids != detail_ids || s.detail_parents != parents {
            s.detail_stop.cancel();
            s.detail_stop = CancellationToken::new();
            self.detail_dirty = detail_ids.clone();
            s.detail_ids = detail_ids;
            s.detail_parents = parents;
        }
        if s.selected != selected {
            s.selected = selected.clone();
            if let Some(id) = selected.filter(|id| !id.starts_with("draft:")) {
                self.changed(&id);
            }
        }
        Ok(())
    }
    pub fn begin_page(
        &mut self,
        generation: u64,
        filter: String,
    ) -> Result<Option<CancellationToken>, ApiError> {
        self.begin_page_view(generation, filter, BrowserMode::List, None)
    }
    pub fn begin_page_view(
        &mut self,
        generation: u64,
        filter: String,
        mode: BrowserMode,
        root: Option<String>,
    ) -> Result<Option<CancellationToken>, ApiError> {
        if self
            .last_generation
            .is_some_and(|last| generation < last || (generation == last && !self.last_visible))
            || self.scope.as_ref().is_some_and(|s| {
                s.generation == generation
                    && (s.filter != filter || s.mode != mode || s.descendant_root != root)
            })
        {
            return Ok(None);
        }
        let (selected, details, parents) = self
            .scope
            .as_ref()
            .filter(|s| s.generation == generation)
            .map(|s| {
                (
                    s.selected.clone(),
                    s.detail_ids.iter().cloned().collect(),
                    s.detail_parents.iter().cloned().collect(),
                )
            })
            .unwrap_or_default();
        self.observe_view(
            generation, filter, selected, true, mode, root, details, parents,
        )?;
        Ok(self.scope.as_ref().map(|s| s.stop.child_token()))
    }
    pub fn adopt_scope(
        &mut self,
        generation: u64,
        read_sequence: u64,
        scope: &super::lineage::DescendantSnapshot,
    ) {
        if let Some(s) = &mut self.scope
            && s.generation == generation
            && read_sequence >= s.scope_sequence
            && s.descendant_root.as_deref() == Some(scope.root.as_str())
        {
            s.cached_scope = Some((
                scope.available,
                scope
                    .threads
                    .iter()
                    .filter(|id| *id != &scope.root)
                    .take(256)
                    .cloned()
                    .collect(),
            ));
            s.scope_sequence = read_sequence;
        }
    }
    pub fn adopt_page(
        &mut self,
        generation: u64,
        read_sequence: u64,
        rows: &[ThreadSummary],
        scope: Option<&super::lineage::DescendantSnapshot>,
    ) {
        self.adopt_patch(generation, read_sequence, rows, scope);
    }
    pub fn adopt_patch(
        &mut self,
        generation: u64,
        read_sequence: u64,
        rows: &[ThreadSummary],
        scope: Option<&super::lineage::DescendantSnapshot>,
    ) {
        if let Some(scope) = scope {
            self.adopt_scope(generation, read_sequence, scope);
        }
        let Some(s) = self.scope.as_mut().filter(|s| s.generation == generation) else {
            return;
        };
        if s.mode != BrowserMode::Watch {
            return;
        }
        for row in rows {
            if s.known
                .get(&row.thread_id)
                .is_some_and(|seq| *seq > read_sequence)
            {
                continue;
            }
            s.known.insert(row.thread_id.clone(), read_sequence);
        }
        while s.known.len() > 2000 {
            let oldest = s
                .known
                .iter()
                .min_by(|(left, ls), (right, rs)| ls.cmp(rs).then_with(|| left.cmp(right)))
                .map(|(id, _)| id.clone())
                .unwrap();
            s.known.remove(&oldest);
        }
    }
    pub fn changed(&mut self, id: &str) {
        if self.scope.is_none() || self.resync.is_some() || id.starts_with("draft:") {
            return;
        }
        if let Some(s) = &self.scope {
            if s.detail_ids.contains(id) {
                self.detail_dirty.insert(id.into());
            }
            if s.detail_parents.contains(id) {
                self.detail_dirty.extend(s.detail_ids.iter().cloned());
            }
        }
        if self.dirty.contains(id) {
            return;
        }
        if id.is_empty() || id.len() > 256 || self.dirty.len() == LIMIT {
            self.request_resync(
                "会話一覧の変更対象を保持できませんでした。最新のページから再取得します",
            );
            return;
        }
        self.dirty.insert(id.into());
    }
    pub fn request_resync(&mut self, reason: &str) {
        self.output.clear();
        if self.resync.is_some() {
            return;
        }
        if let Some(s) = &self.scope {
            self.resync = Some((s.generation, reason.into()));
            s.stop.cancel();
            s.detail_stop.cancel();
            self.dirty.clear();
            self.detail_dirty.clear();
            self.inflight = None;
        }
    }
    pub fn take_resync(&mut self) -> Option<(u64, String)> {
        if !self.scope.as_ref().is_some_and(|s| s.visible) {
            return None;
        }
        let result = self.resync.take()?;
        self.close();
        self.last_visible = false;
        Some(result)
    }
    pub fn deadline(&self) -> Option<tokio::time::Instant> {
        (self.scope.as_ref().is_some_and(|s| s.visible)
            && self.resync.is_none()
            && (!self.dirty.is_empty() || !self.detail_dirty.is_empty())
            && self.inflight.is_none())
        .then(|| self.next.unwrap_or_else(tokio::time::Instant::now))
    }
    pub fn take(&mut self, now: tokio::time::Instant) -> Option<Ticket> {
        if self.scope.as_ref().is_none_or(|s| !s.visible)
            || self.resync.is_some()
            || (self.dirty.is_empty() && self.detail_dirty.is_empty())
            || self.inflight.is_some()
            || self.next.is_some_and(|at| at > now)
        {
            return None;
        }
        let s = self.scope.as_ref()?;
        let ids = std::mem::take(&mut self.dirty)
            .into_iter()
            .collect::<Vec<_>>();
        let details = std::mem::take(&mut self.detail_dirty)
            .intersection(&s.detail_ids)
            .cloned()
            .collect::<Vec<_>>();
        let selected = s.selected.as_ref().filter(|id| ids.contains(id)).cloned();
        self.serial = self.serial.wrapping_add(1);
        self.inflight = Some((self.serial, ids.clone(), details.clone()));
        self.next = Some(now + PERIOD);
        Some(Ticket {
            generation: s.generation,
            known_requested_ids: ids
                .iter()
                .filter(|id| s.known.contains_key(*id))
                .cloned()
                .collect(),
            ids,
            selected,
            stop: s.stop.child_token(),
            mode: s.mode,
            descendant_root: s.descendant_root.clone(),
            cached_scope: s.cached_scope.clone(),
            detail_ids: details,
            detail_parent_ids: s.detail_parents.iter().cloned().collect(),
            detail_stop: s.detail_stop.child_token(),
            serial: self.serial,
        })
    }
    pub fn complete(&mut self, ticket: &Ticket) -> bool {
        if self.inflight.as_ref().map(|(serial, _, _)| *serial) != Some(ticket.serial)
            || self
                .scope
                .as_ref()
                .is_none_or(|s| s.generation != ticket.generation)
            || ticket.stop.is_cancelled()
        {
            return false;
        }
        self.inflight = None;
        true
    }
    pub fn details_current(&self, ticket: &Ticket) -> bool {
        !ticket.stop.is_cancelled()
            && !ticket.detail_stop.is_cancelled()
            && self
                .scope
                .as_ref()
                .is_some_and(|s| s.visible && s.generation == ticket.generation)
    }
    pub fn failed(&mut self, ticket: &Ticket) {
        if ticket.stop.is_cancelled()
            || self
                .scope
                .as_ref()
                .is_none_or(|s| s.generation != ticket.generation)
            || self
                .inflight
                .as_ref()
                .is_some_and(|(serial, _, _)| *serial != ticket.serial)
        {
            return;
        }
        self.inflight = None;
        for id in &ticket.ids {
            self.changed(id);
        }
        if self.details_current(ticket) {
            self.detail_dirty.extend(ticket.detail_ids.iter().cloned());
        }
    }
}

pub(super) struct WatchDetailsBatch {
    pub details: Vec<crate::ui::watch::PreparedWatchDetail>,
    pub missing_ids: Vec<String>,
}
pub(super) struct Observation {
    pub patch: Option<crate::ui::thread_patch::BrowserPatch>,
    pub details: Option<WatchDetailsBatch>,
}
pub(super) struct RawObservation {
    mode: BrowserMode,
    patch: Option<super::browser_read::RawPatch>,
    details: Option<super::watch_read::RawWatch>,
    detail_ids: Vec<String>,
    title_only: BTreeSet<String>,
}

pub(super) fn read_tx(
    db: &rusqlite::Connection,
    ticket: &Ticket,
    allowed_scope: Option<(bool, Vec<String>)>,
) -> Result<RawObservation, crate::storage::StorageError> {
    use crate::control::db::thread_summary;
    if ticket.ids.len() > LIMIT
        || ticket.detail_ids.len() > LIMIT
        || ticket.detail_parent_ids.len() > LIMIT
        || ticket.known_requested_ids.len() > LIMIT
    {
        return Err(ApiError::new(ErrorCode::InputLimit, "Browser scope exceeds128 IDs").into());
    }
    let allowed = allowed_scope.or_else(|| ticket.cached_scope.clone());
    if allowed.as_ref().is_some_and(|(_, ids)| ids.len() > 256) {
        return Err(ApiError::new(ErrorCode::InputLimit, "Browser lineage exceeds256 IDs").into());
    }
    let scoped = ticket.mode == BrowserMode::Watch && ticket.descendant_root.is_some();
    let permitted = |id: &String| {
        !scoped
            || allowed.as_ref().is_some_and(|(available, ids)| {
                *available && ids.contains(id) && Some(id) != ticket.descendant_root.as_ref()
            })
    };
    let requested = ticket
        .ids
        .iter()
        .filter(|id| permitted(id) || ticket.detail_parent_ids.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    let mut rows = thread_summary::read_ids_tx(db, &requested)?;
    let found = rows
        .iter()
        .map(|row| row.thread_id.clone())
        .collect::<BTreeSet<_>>();
    let missing = ticket
        .ids
        .iter()
        .filter(|id| !found.contains(*id))
        .cloned()
        .collect();
    let mut title_only = BTreeSet::new();
    if ticket.mode == BrowserMode::Watch {
        rows.retain(|row| {
            let phase = thread_summary::coarse_activity(&row.activity);
            let candidate = !matches!(
                phase,
                crate::activity::ActivityPhase::Idle | crate::activity::ActivityPhase::Stopped
            ) && (phase != crate::activity::ActivityPhase::Completed
                || row.identity.answer_id.is_some());
            let parent_only = ticket.detail_parent_ids.contains(&row.thread_id)
                && (!permitted(&row.thread_id)
                    || (!candidate && !ticket.known_requested_ids.contains(&row.thread_id)));
            if parent_only {
                title_only.insert(row.thread_id.clone());
            }
            parent_only || candidate || ticket.known_requested_ids.contains(&row.thread_id)
        });
    }
    let patch = if ticket.ids.is_empty() {
        None
    } else {
        Some(super::browser_read::read_shared_tx(
            db,
            rows,
            missing,
            if ticket.mode == BrowserMode::List {
                ticket.selected.as_deref()
            } else {
                None
            },
        )?)
    };
    let detail_ids = ticket
        .detail_ids
        .iter()
        .filter(|id| permitted(id))
        .cloned()
        .collect::<Vec<_>>();
    let details = if detail_ids.is_empty() || ticket.detail_stop.is_cancelled() {
        None
    } else {
        let empty = BTreeSet::new();
        let known = patch.as_ref().map(|p| p.rows()).unwrap_or(&[]);
        let titles = patch.as_ref().map(|p| p.title_ids()).unwrap_or(empty);
        Some(super::watch_read::read_shared_tx(
            db,
            &detail_ids,
            known,
            &titles,
        )?)
    };
    Ok(RawObservation {
        mode: ticket.mode,
        patch,
        details,
        detail_ids,
        title_only,
    })
}
pub(super) fn prepare(raw: RawObservation, read_sequence: u64, generation: u64) -> Observation {
    let prepared = raw.patch.map(|patch| {
        super::browser_read::prepare_with_watch(patch, raw.mode == BrowserMode::Watch)
    });
    let titles = prepared
        .as_ref()
        .map(|p| {
            p.rows
                .iter()
                .map(|row| (row.thread_id.clone(), row.title.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let details = raw.details.map(|details| {
        let details = super::watch_read::prepare_with_titles(details, &titles);
        let found = details
            .iter()
            .map(|d| d.facts().thread_id.as_str())
            .collect::<BTreeSet<_>>();
        let missing_ids = raw
            .detail_ids
            .into_iter()
            .filter(|id| !found.contains(id.as_str()))
            .collect();
        WatchDetailsBatch {
            details,
            missing_ids,
        }
    });
    let patch = prepared.map(|mut p| {
        p.rows
            .retain(|row| !raw.title_only.contains(&row.thread_id));
        p.watch_summaries
            .retain(|row| !raw.title_only.contains(&row.thread_id));
        crate::ui::thread_patch::BrowserPatch {
            generation,
            read_sequence,
            display_times: thread_display_times(&p.rows),
            rows: p.rows,
            missing_ids: p.missing_ids,
            limited_titles: p.limited_title_ids,
            excerpt: p.selected_excerpt,
            watch_summaries: p.watch_summaries,
        }
    });
    Observation { patch, details }
}

pub(super) async fn read(
    control: &Control,
    ticket: &Ticket,
    progress_slots: Arc<Semaphore>,
) -> Result<Observation, ApiError> {
    let scope = ticket.clone();
    let permit = tokio::select! {biased;
        _=ticket.stop.cancelled()=>return Err(observation_closed()),
        permit=progress_slots.acquire_owned()=>permit.map_err(|_|observation_closed())?,
    };
    let result = super::snapshot::read_sequenced(
        control.store.database.path().to_path_buf(),
        ticket.stop.clone(),
        move |db, _sequence| read_tx(db, &scope, None),
    )
    .await;
    drop(permit);
    let (raw, read_sequence) = checked_storage(control, result).await?;
    if ticket.stop.is_cancelled() {
        return Err(observation_closed());
    }
    project(raw, read_sequence, ticket.generation).await
}

fn observation_closed() -> ApiError {
    ApiError::new(ErrorCode::InvalidState, "Browser observation closed")
}

pub(super) async fn project(
    raw: RawObservation,
    read_sequence: u64,
    generation: u64,
) -> Result<Observation, ApiError> {
    presentation_work(move || prepare(raw, read_sequence, generation)).await
}

pub(super) async fn page(
    control: &Control,
    args: ThreadListArgs,
    stop: CancellationToken,
    progress_slots: Arc<Semaphore>,
) -> Result<(Page<ThreadSummary>, Vec<ThreadDisplayTime>, u64), ApiError> {
    let permit = tokio::select! {biased;
        _=stop.cancelled()=>return Err(observation_closed()),
        permit=progress_slots.acquire_owned()=>permit.map_err(|_|observation_closed())?,
    };
    let result = super::snapshot::read_sequenced(
        control.store.database.path().to_path_buf(),
        stop.clone(),
        move |db, _sequence| crate::control::db::thread_list(db, &args).map_err(Into::into),
    )
    .await;
    drop(permit);
    let (page, sequence) = checked_storage(control, result).await?;
    if stop.is_cancelled() {
        return Err(observation_closed());
    }
    presentation_work(move || {
        let times = thread_display_times(&page.items);
        (page, times, sequence)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalesces_exact_ids_and_keeps_one_flight_with_a_global_start_deadline() {
        let mut browser = Browser::default();
        browser.changed("hidden");
        assert!(browser.deadline().is_none());
        browser.observe(1, "".into(), None, true).unwrap();
        for _ in 0..100 {
            browser.changed("changed");
        }
        let now = tokio::time::Instant::now();
        let ticket = browser.take(now).unwrap();
        assert_eq!(ticket.ids, vec!["changed"]);
        browser.changed("next");
        assert!(browser.take(now + PERIOD).is_none());
        assert!(browser.complete(&ticket));
        assert!(
            browser
                .take(now + PERIOD - Duration::from_millis(1))
                .is_none()
        );
        let next = browser.take(now + PERIOD).unwrap();
        assert_eq!(next.ids, vec!["next"]);
        browser.observe(1, "".into(), None, false).unwrap();
        assert!(next.stop.is_cancelled() && !browser.complete(&next));
        browser
            .observe(2, "different".into(), Some("selected".into()), true)
            .unwrap();
        assert!(
            browser.take(now + PERIOD).is_none(),
            "switch cannot bypass budget"
        );
        assert_eq!(
            browser.take(now + 2 * PERIOD).unwrap().ids,
            vec!["selected"]
        );
    }
    #[test]
    fn covered_scope_keeps_bounded_changes_without_reads_and_rearms_on_resume() {
        let mut browser = Browser::default();
        browser
            .observe(3, "".into(), Some("selected".into()), true)
            .unwrap();
        let now = tokio::time::Instant::now();
        let running = browser.take(now).unwrap();
        browser
            .observe(3, "".into(), Some("selected".into()), false)
            .unwrap();
        browser.changed("unloaded-addition");
        assert!(running.stop.is_cancelled() && !browser.complete(&running));
        assert!(browser.deadline().is_none() && browser.take(now + PERIOD).is_none());
        browser
            .observe(3, "".into(), Some("selected".into()), true)
            .unwrap();
        let resumed = browser.take(now + PERIOD).unwrap();
        assert_eq!(resumed.ids, vec!["selected", "unloaded-addition"]);
        assert!(!resumed.stop.is_cancelled());
    }
    #[test]
    fn delayed_page_cannot_reactivate_a_hidden_or_retired_scope() {
        let mut browser = Browser::default();
        let active = browser.begin_page(3, "a".into()).unwrap().unwrap();
        browser.observe(3, "a".into(), None, false).unwrap();
        assert!(active.is_cancelled());
        assert!(browser.begin_page(3, "a".into()).unwrap().is_none());
        browser.observe(3, "a".into(), None, true).unwrap();
        assert!(
            browser
                .begin_page(3, "wrong filter".into())
                .unwrap()
                .is_none()
        );
        assert!(browser.begin_page(3, "a".into()).unwrap().is_some());
        browser.request_resync("lost IDs");
        browser.take_resync().unwrap();
        assert!(browser.begin_page(3, "a".into()).unwrap().is_none());
        assert!(browser.begin_page(4, "a".into()).unwrap().is_some());
    }
    #[test]
    fn unseen_additions_are_observed_and_overflow_requests_one_resync() {
        let mut browser = Browser::default();
        let _page = browser.begin_page(7, "filter".into()).unwrap();
        for id in 0..128 {
            browser.changed(&format!("unloaded-{id}"));
        }
        assert_eq!(browser.dirty.len(), 128);
        browser.changed("overflow");
        assert!(browser.deadline().is_none() && browser.dirty.is_empty());
        assert_eq!(browser.take_resync().unwrap().0, 7);
        assert!(browser.take_resync().is_none());
        browser.observe(6, "old".into(), None, true).unwrap();
        assert!(browser.scope.is_none());
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use crate::activity::ActivityPhase;
    fn observe(browser: &mut Browser, ids: &[&str], parents: &[&str]) {
        browser
            .observe_view(
                1,
                "".into(),
                None,
                true,
                BrowserMode::Watch,
                None,
                ids.iter().map(|id| (*id).into()).collect(),
                parents.iter().map(|id| (*id).into()).collect(),
            )
            .unwrap();
    }
    #[test]
    fn viewport_retires_only_details_and_failure_requeues_current_bounded_work() {
        let mut browser = Browser::default();
        observe(&mut browser, &["first"], &["parent"]);
        browser.changed("metadata");
        let now = tokio::time::Instant::now();
        let old = browser.take(now).unwrap();
        assert_eq!(old.detail_ids, ["first"]);
        observe(&mut browser, &["second"], &["other-parent"]);
        assert!(!old.stop.is_cancelled());
        assert!(old.detail_stop.is_cancelled());
        assert!(browser.complete(&old));
        assert!(!browser.details_current(&old));
        let fresh = browser.take(now + PERIOD).unwrap();
        assert_eq!(fresh.detail_ids, ["second"]);
        browser.failed(&fresh);
        let retry = browser.take(now + 2 * PERIOD).unwrap();
        assert_eq!(retry.detail_ids, ["second"]);
        assert!(browser.complete(&retry));
        assert!(browser.details_current(&retry));
        browser.changed("unrelated");
        let unrelated = browser.take(now + 3 * PERIOD).unwrap();
        assert!(unrelated.detail_ids.is_empty());
        browser.complete(&unrelated);
        browser.changed("other-parent");
        assert_eq!(
            browser.take(now + 4 * PERIOD).unwrap().detail_ids,
            ["second"]
        );
    }
    #[tokio::test(start_paused = true)]
    async fn ticket_copies_only_requested_known_ids_and_scope_uses_actual_threads() {
        let mut browser = Browser::default();
        observe(&mut browser, &[], &[]);
        let scope = browser.scope.as_mut().unwrap();
        for i in 0..2000 {
            scope.known.insert(format!("t{i}"), 1);
        }
        browser.changed("t42");
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        assert_eq!(ticket.known_requested_ids, ["t42"]);
        assert_eq!(ticket.ids.len(), 1);
        browser.complete(&ticket);
        browser
            .observe_view(
                2,
                "".into(),
                None,
                true,
                BrowserMode::Watch,
                Some("root".into()),
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
        let snapshot = super::super::lineage::DescendantSnapshot {
            root: "root".into(),
            available: true,
            active: 0,
            recovery: 0,
            truncated: true,
            cycle: false,
            threads: vec!["child".into()],
            members: vec![
                "root".into(),
                "child".into(),
                "deleted-bridge".into(),
                "sentinel".into(),
            ],
        };
        browser.adopt_scope(2, 8, &snapshot);
        let mut old = snapshot.clone();
        old.threads = vec!["stale".into()];
        browser.adopt_scope(2, 7, &old);
        browser.changed("child");
        tokio::time::advance(PERIOD).await;
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        assert_eq!(ticket.cached_scope, Some((true, vec!["child".into()])));
    }
    fn fixture() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        for id in [
            "parent", "child", "failed", "idle", "stopped", "noanswer", "answered",
        ] {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,?1,'/','','p','m','now')",[id]).unwrap();
        }
        for (id, state) in [
            ("parent", "failed"),
            ("child", "live"),
            ("failed", "failed"),
            ("stopped", "stopped"),
            ("noanswer", "completed"),
            ("answered", "completed"),
        ] {
            db.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES(?1,?2,?3,'2020-01-01T00:00:00Z')",rusqlite::params![format!("r-{id}"),id,state]).unwrap();
        }
        db.execute_batch("UPDATE thread SET parent_thread_id='parent' WHERE id='child'; INSERT INTO node(id,kind,created_at,payload_json)VALUES('answer','assistant','now','{}'); INSERT INTO turn(id,thread_id,run_id,state,created_at,answer_node_id)VALUES('u','answered','r-answered','completed','now','answer');").unwrap();
        db
    }
    #[test]
    fn list_observation_keeps_the_e_read_shape_without_watch_projection() {
        let db = fixture();
        let mut browser = Browser::default();
        browser
            .observe(1, String::new(), Some("child".into()), true)
            .unwrap();
        browser.changed("failed");
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let raw = read_tx(&db, &ticket, None).unwrap();
        assert_eq!(
            trace.count(),
            2,
            "List metadata plus selected excerpt, without Watch detail work"
        );
        drop(trace);
        let observation = prepare(raw, 1, 1);
        assert!(observation.details.is_none());
        let patch = observation.patch.unwrap();
        assert!(patch.watch_summaries.is_empty());
        assert_eq!(
            patch
                .rows
                .iter()
                .map(|row| row.thread_id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["child", "failed"])
        );
        assert_eq!(patch.excerpt.as_ref().unwrap().0, "child");
    }
    #[tokio::test(start_paused = true)]
    async fn new_global_changes_admit_only_candidates_and_parent_title_dependencies_never_add_rows()
    {
        let db = fixture();
        // This parent contributes a title, but has no saved answer/work of its own.
        db.execute(
            "UPDATE run SET state='completed' WHERE thread_id='parent'",
            [],
        )
        .unwrap();
        let mut browser = Browser::default();
        observe(&mut browser, &["child"], &["parent"]);
        for id in [
            "parent", "child", "failed", "idle", "stopped", "noanswer", "answered",
        ] {
            browser.changed(id);
        }
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let result = prepare(read_tx(&db, &ticket, None).unwrap(), 1, 1);
        let patch = result.patch.unwrap();
        assert_eq!(
            patch
                .rows
                .iter()
                .map(|row| row.thread_id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["child", "failed", "answered"])
        );
        assert_eq!(
            patch
                .watch_summaries
                .iter()
                .find(|row| row.thread_id == "answered")
                .unwrap()
                .answer_id
                .as_deref(),
            Some("answer")
        );
        assert_eq!(
            result.details.unwrap().details[0]
                .facts()
                .parent_title
                .as_deref(),
            Some("parent")
        );
        browser.adopt_patch(1, 1, &patch.rows, None);
        assert!(!browser.scope.as_ref().unwrap().known.contains_key("parent"));
        browser.complete(&ticket);
        browser
            .scope
            .as_mut()
            .unwrap()
            .known
            .insert("stopped".into(), 1);
        browser.changed("stopped");
        tokio::time::advance(PERIOD).await;
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let patch = prepare(read_tx(&db, &ticket, None).unwrap(), 2, 1)
            .patch
            .unwrap();
        assert_eq!(patch.rows[0].activity, ActivityPhase::Stopped);
    }
    #[test]
    fn watch_selected_row_reuses_summary_without_reading_conversation_excerpt() {
        let db = fixture();
        let mut browser = Browser::default();
        browser
            .observe_view(
                1,
                "".into(),
                Some("child".into()),
                true,
                BrowserMode::Watch,
                None,
                vec!["child".into()],
                vec!["parent".into()],
            )
            .unwrap();
        let now = tokio::time::Instant::now();
        let ticket = browser.take(now).unwrap();
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let raw = read_tx(&db, &ticket, None).unwrap();
        assert_eq!(
            trace.count(),
            2,
            "one shared summary and one visible detail batch, no selected history read"
        );
        drop(trace);
        let observation = prepare(raw, 1, 1);
        assert!(observation.patch.unwrap().excerpt.is_none());
        assert_eq!(observation.details.unwrap().details.len(), 1);
        browser.complete(&ticket);
        browser
            .observe_view(
                1,
                "".into(),
                Some("child".into()),
                false,
                BrowserMode::Watch,
                None,
                vec!["child".into()],
                vec!["parent".into()],
            )
            .unwrap();
        browser
            .observe_view(
                1,
                "".into(),
                Some("child".into()),
                true,
                BrowserMode::Watch,
                None,
                vec!["child".into()],
                vec!["parent".into()],
            )
            .unwrap();
        let resumed=browser.take(now+PERIOD).expect("resuming refetches details even when an old completed delivery was discarded by the UI");
        assert_eq!(resumed.detail_ids, ["child"]);
    }
    #[test]
    fn eligible_parent_dependency_is_also_a_global_watch_candidate() {
        let db = fixture();
        let mut browser = Browser::default();
        observe(&mut browser, &["child"], &["parent"]);
        browser.changed("parent");
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let observation = prepare(read_tx(&db, &ticket, None).unwrap(), 1, 1);
        let patch = observation.patch.unwrap();
        assert_eq!(patch.rows.len(), 1);
        assert_eq!(patch.rows[0].thread_id, "parent");
        assert_eq!(patch.rows[0].activity, ActivityPhase::Failed);
        assert_eq!(
            observation.details.unwrap().details[0]
                .facts()
                .parent_title
                .as_deref(),
            Some("parent")
        );
    }
    #[test]
    fn scoped_reads_exclude_root_and_unrelated_metadata_but_keep_parent_title() {
        let db = fixture();
        let mut browser = Browser::default();
        browser
            .observe_view(
                1,
                "".into(),
                None,
                true,
                BrowserMode::Watch,
                Some("parent".into()),
                vec!["child".into(), "failed".into()],
                vec!["parent".into()],
            )
            .unwrap();
        for id in ["parent", "child", "failed"] {
            browser.changed(id);
        }
        browser
            .scope
            .as_mut()
            .unwrap()
            .known
            .insert("failed".into(), 0);
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let result = prepare(
            read_tx(&db, &ticket, Some((true, vec!["child".into()]))).unwrap(),
            1,
            1,
        );
        assert!(
            result
                .patch
                .as_ref()
                .unwrap()
                .missing_ids
                .contains(&"failed".into()),
            "new scope explicitly retires the previously loaded row"
        );
        assert_eq!(
            result
                .patch
                .unwrap()
                .rows
                .iter()
                .map(|row| row.thread_id.as_str())
                .collect::<Vec<_>>(),
            ["child"]
        );
        let details = result.details.unwrap();
        assert_eq!(details.details.len(), 1);
        assert_eq!(
            details.details[0].facts().parent_title.as_deref(),
            Some("parent")
        );
    }
    #[test]
    fn oversized_tickets_fail_before_database_reads_and_empty_watch_has_no_queries() {
        let db = fixture();
        let mut browser = Browser::default();
        observe(&mut browser, &[], &[]);
        browser.changed("child");
        let mut ticket = browser.take(tokio::time::Instant::now()).unwrap();
        ticket.ids = vec!["child".into(); 129];
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        assert!(read_tx(&db, &ticket, None).is_err());
        assert_eq!(trace.count(), 0);
        drop(trace);
        ticket.ids.clear();
        let trace = crate::frontend::sql_trace::Statements::new(&db);
        let raw = read_tx(&db, &ticket, None).unwrap();
        assert_eq!(trace.count(), 0);
        drop(trace);
        let result = prepare(raw, 1, 1);
        assert!(result.patch.is_none() && result.details.is_none());
    }
    fn shell_detail(
        thread: &str,
        execution: &str,
        at: std::time::Instant,
    ) -> crate::ui::watch::PreparedWatchDetail {
        use crate::ui::{
            live_work::{LiveWorkRow, LiveWorkSnapshot, LiveWorkTarget},
            watch::WatchDetail,
        };
        WatchDetail {
            thread_id: thread.into(),
            latest_run_id: Some(format!("run-{thread}")),
            answer_id: None,
            turn_id: None,
            turn_generation: None,
            phase: ActivityPhase::Background,
            parent_title: None,
            state_age: None,
            model_request_age: None,
            recovery_reason: None,
            answer_preview: None,
            pending_messages: 0,
            revisit: None,
            work: Some(LiveWorkSnapshot {
                run_id: format!("run-{thread}"),
                shells: 1,
                children: 0,
                waits: Some(0),
                overflow: 0,
                observed_at: at,
                rows: vec![LiveWorkRow {
                    target: LiveWorkTarget::Shell {
                        execution_id: execution.into(),
                        turn_id: None,
                    },
                    label: "shell".into(),
                    state: "running".into(),
                    started_at: None,
                    elapsed_base: None,
                    last_output_at: None,
                    output_age_base: None,
                }],
            }),
        }
        .prepare()
    }
    #[test]
    fn output_hints_dirty_only_current_detail_owner_and_never_metadata_or_hidden_consumers() {
        let mut browser = Browser::default();
        observe(&mut browser, &["a", "b"], &[]);
        let now = tokio::time::Instant::now();
        let at = std::time::Instant::now();
        let ticket = browser.take(now).unwrap();
        browser.complete(&ticket);
        browser.accept_details(
            &ticket,
            &[shell_detail("a", "sa", at), shell_detail("b", "sb", at)],
        );
        assert_eq!(browser.take_output_ids(now).unwrap(), ["sa", "sb"]);
        browser.output_markers(vec![
            ("sa".into(), Some(at + Duration::from_millis(1))),
            ("sb".into(), Some(at)),
        ]);
        assert!(browser.dirty.is_empty());
        assert_eq!(browser.detail_dirty, BTreeSet::from(["a".into()]));
        let refresh = browser.take(now + PERIOD).unwrap();
        assert!(refresh.ids.is_empty());
        assert_eq!(refresh.detail_ids, ["a"]);
        browser.complete(&refresh);
        observe(&mut browser, &["b"], &[]);
        browser.output_notified();
        assert!(
            browser
                .take_output_ids(now + PERIOD - Duration::from_millis(1))
                .is_none()
        );
        assert_eq!(browser.take_output_ids(now + PERIOD).unwrap(), ["sb"]);
        browser.output_markers(vec![("sa".into(), Some(at + Duration::from_secs(1)))]);
        assert_eq!(
            browser.detail_dirty,
            BTreeSet::from(["b".into()]),
            "viewport re-read, not retired shell output"
        );
        browser
            .observe_view(
                1,
                "".into(),
                None,
                false,
                BrowserMode::Watch,
                None,
                vec!["b".into()],
                vec![],
            )
            .unwrap();
        for _ in 0..40 {
            browser.output_notified();
            assert!(browser.output_deadline().is_none());
        }
        browser.accept_details(&refresh, &[shell_detail("a", "sa", at)]);
        assert!(browser.take_output_ids(now + 2 * PERIOD).is_none());
    }
}
