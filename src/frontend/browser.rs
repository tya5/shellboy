//! One bounded visible-browser observer on existing frontend read admission.
use super::*;
use std::collections::BTreeSet;

const LIMIT: usize = 128;
const PERIOD: Duration = Duration::from_millis(500);
struct Scope {
    generation: u64,
    filter: String,
    selected: Option<String>,
    visible: bool,
    stop: CancellationToken,
}
pub(super) struct Ticket {
    pub generation: u64,
    pub ids: Vec<String>,
    pub selected: Option<String>,
    pub stop: CancellationToken,
    serial: u64,
}
#[derive(Default)]
pub(super) struct Browser {
    scope: Option<Scope>,
    last_generation: Option<u64>,
    last_visible: bool,
    dirty: BTreeSet<String>,
    inflight: Option<(u64, Vec<String>)>,
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
    fn close(&mut self) {
        if let Some(scope) = self.scope.take() {
            scope.stop.cancel();
        }
        self.dirty.clear();
        self.inflight = None;
    }
    pub fn observe(
        &mut self,
        generation: u64,
        filter: String,
        selected: Option<String>,
        visible: bool,
    ) -> Result<(), ApiError> {
        if self.last_generation.is_some_and(|last| generation < last) {
            return Ok(());
        }
        if filter.chars().count() > 256
            || selected
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 256)
        {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Invalid browser observation scope",
            ));
        }
        self.last_generation = Some(generation);
        self.last_visible = visible;
        if !visible {
            if let Some(scope) = &mut self.scope {
                scope.visible = false;
                scope.stop.cancel();
            }
            if let Some((_, ids)) = self.inflight.take() {
                for id in ids {
                    self.changed(&id);
                }
            }
            return Ok(());
        }
        let replace = self
            .scope
            .as_ref()
            .is_none_or(|scope| scope.generation != generation || scope.filter != filter);
        if replace {
            self.close();
            self.resync = None;
            self.scope = Some(Scope {
                generation,
                filter,
                selected: None,
                visible: true,
                stop: CancellationToken::new(),
            });
        }
        let scope = self.scope.as_mut().expect("visible browser owns scope");
        if !scope.visible {
            scope.stop = CancellationToken::new();
            scope.visible = true;
        }
        if scope.selected != selected {
            scope.selected = selected.clone();
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
        if self
            .last_generation
            .is_some_and(|last| generation < last || (generation == last && !self.last_visible))
            || self
                .scope
                .as_ref()
                .is_some_and(|s| s.generation == generation && s.filter != filter)
        {
            return Ok(None);
        }
        let selected = self
            .scope
            .as_ref()
            .filter(|s| s.generation == generation)
            .and_then(|s| s.selected.clone());
        self.observe(generation, filter, selected, true)?;
        Ok(self.scope.as_ref().map(|scope| scope.stop.child_token()))
    }
    pub fn changed(&mut self, id: &str) {
        if self.scope.is_none() || self.resync.is_some() || id.starts_with("draft:") {
            return;
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
        self.dirty.insert(id.to_owned());
    }
    pub fn request_resync(&mut self, reason: &str) {
        if self.resync.is_some() {
            return;
        }
        if let Some(scope) = &self.scope {
            self.resync = Some((scope.generation, reason.to_owned()));
            scope.stop.cancel();
            self.dirty.clear();
            self.inflight = None;
        }
    }
    pub fn take_resync(&mut self) -> Option<(u64, String)> {
        if !self.scope.as_ref().is_some_and(|s| s.visible) {
            return None;
        }
        let value = self.resync.take()?;
        self.close();
        self.last_visible = false;
        Some(value)
    }
    pub fn deadline(&self) -> Option<tokio::time::Instant> {
        (self.scope.as_ref().is_some_and(|s| s.visible)
            && self.resync.is_none()
            && !self.dirty.is_empty()
            && self.inflight.is_none())
        .then(|| self.next.unwrap_or_else(tokio::time::Instant::now))
    }
    pub fn take(&mut self, now: tokio::time::Instant) -> Option<Ticket> {
        if self.scope.as_ref().is_none_or(|s| !s.visible)
            || self.resync.is_some()
            || self.dirty.is_empty()
            || self.inflight.is_some()
            || self.next.is_some_and(|at| at > now)
        {
            return None;
        }
        let scope = self.scope.as_ref()?;
        let ids = std::mem::take(&mut self.dirty)
            .into_iter()
            .collect::<Vec<_>>();
        let selected = scope
            .selected
            .as_ref()
            .filter(|id| ids.contains(id))
            .cloned();
        self.serial = self.serial.wrapping_add(1);
        self.inflight = Some((self.serial, ids.clone()));
        self.next = Some(now + PERIOD);
        Some(Ticket {
            generation: scope.generation,
            ids,
            selected,
            stop: scope.stop.child_token(),
            serial: self.serial,
        })
    }
    pub fn complete(&mut self, ticket: &Ticket) -> bool {
        if self.inflight.as_ref().map(|(serial, _)| *serial) != Some(ticket.serial)
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
}

pub(super) async fn read(
    control: &Control,
    ticket: &Ticket,
    progress_slots: Arc<Semaphore>,
) -> Result<crate::ui::thread_patch::BrowserPatch, ApiError> {
    let ids = ticket.ids.clone();
    let selected = ticket.selected.clone();
    let permit = tokio::select! {biased;
        _=ticket.stop.cancelled()=>return Err(observation_closed()),
        permit=progress_slots.acquire_owned()=>permit.map_err(|_|observation_closed())?,
    };
    let result = super::snapshot::read_sequenced(
        control.store.database.path().to_path_buf(),
        ticket.stop.clone(),
        move |db, _sequence| super::browser_read::read_tx(db, &ids, selected.as_deref()),
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
    raw: super::browser_read::RawPatch,
    read_sequence: u64,
    generation: u64,
) -> Result<crate::ui::thread_patch::BrowserPatch, ApiError> {
    presentation_work(move || {
        let prepared = super::browser_read::prepare(raw);
        let display_times = thread_display_times(&prepared.rows);
        crate::ui::thread_patch::BrowserPatch {
            generation,
            read_sequence,
            rows: prepared.rows,
            display_times,
            missing_ids: prepared.missing_ids,
            limited_titles: prepared.limited_title_ids,
            excerpt: prepared.selected_excerpt,
        }
    })
    .await
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_page_and_id_patch_complete_while_writer_queue_is_held() {
        let root = tempfile::tempdir().unwrap();
        let owner =
            crate::app::DataOwner::start(None, Some(root.path().join("data")), root.path().into())
                .await
                .unwrap();
        owner.db.call(|db| {
            db.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('browser-root','saved browser name','/','','p','m','2026-01-01T00:00:00Z')",[])?;Ok(())
        }).await.unwrap();
        let control = Control::new(&owner, root.path().into());
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let pending = owner
            .db
            .try_call(move |_| {
                let _ = entered.send(());
                let _ = blocked.recv();
                Ok(())
            })
            .unwrap();
        crate::test_watchdog::watchdog("E readonly observation", ready)
            .await
            .unwrap()
            .unwrap();
        let slots = Arc::new(Semaphore::new(1));
        let mut browser = Browser::default();
        browser
            .observe(1, "".into(), Some("browser-root".into()), true)
            .unwrap();
        let ticket = browser.take(tokio::time::Instant::now()).unwrap();
        let observed = crate::test_watchdog::watchdog("E readonly observation", async {
            let (page, _, page_sequence) = page(
                &control,
                ThreadListArgs::default(),
                CancellationToken::new(),
                slots.clone(),
            )
            .await?;
            let patch = read(&control, &ticket, slots.clone()).await?;
            Ok::<_, ApiError>((page, page_sequence, patch))
        })
        .await;
        drop(release);
        pending.receive().await.unwrap();
        let (page, page_sequence, patch) = observed.unwrap().unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].title, "saved browser name");
        assert_eq!(patch.rows[0].thread_id, "browser-root");
        assert_eq!(patch.rows[0].title, page.items[0].title);
        assert!(patch.read_sequence > page_sequence);
        assert_eq!(slots.available_permits(), 1);
        drop(control);
        owner.shutdown().await.unwrap();
    }
}
