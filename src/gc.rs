//! Bounded raw retention and revision-checked history reachability collection.
use crate::maintenance::{Work, WorkSet};
use crate::storage::{Background, BackgroundRequester, StorageError};
use crate::{
    attachments::{AttachmentService, BlockingFiles},
    config::Config,
    protocol::{ApiError, ErrorCode, StorageCategory, StorageStatus},
    storage::DbWorker,
};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{num::NonZeroU32, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

mod event_retention;
mod history;
pub(crate) mod observation;
mod usage;
use history::history_step;
use usage::{logical_usage, physical_usage};

type Result<T> = std::result::Result<T, ApiError>;
const BATCH: usize = 128;
const RAW_BATCH_BYTES: u64 = 1024 * 1024;
const RAW_PREFIX: &str = "raw_tombstone.";

#[derive(Clone)]
pub struct GcService {
    database: DbWorker,
    config: Arc<Config>,
    data_dir: PathBuf,
    attachments: Arc<AttachmentService>,
    files: BlockingFiles,
    /// The GC background requester: one outstanding unit (storage design #171).
    requester: Arc<Mutex<BackgroundRequester>>,
    raw_cursor: Arc<std::sync::Mutex<RawCursor>>,
    /// Round-robin position of dead-delivery/shell-event retention (#170).
    event_cursor: Arc<std::sync::Mutex<event_retention::Cursor>>,
    // One coalesced permit for the existing frontend; never raw data or a task.
    availability_changed: Arc<Notify>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct CollectionReport {
    pub raw_bytes_collected: u64,
    pub nodes_tombstoned: u64,
    pub history_bytes_collected: u64,
    pub protected_raw_bytes: u64,
    pub managed_deleted: Vec<String>,
    pub managed_deleted_count: u64,
    pub managed_failure_count: u64,
    pub managed_details_truncated: bool,
    pub managed_failures: Vec<(String, String)>,
    pub managed_protected_bytes: u64,
    pub index_sources_collected: u64,
    pub inactive_index_spaces_collected: u64,
    pub submitted_drafts_collected: u64,
    pub summary_cache_parts_collected: u64,
    pub summary_cache_bytes_collected: u64,
    pub deferred: bool,
    /// Kinds whose bounded unit was deferred at the DB step budget; the
    /// maintenance requester re-marks them (storage design #171).
    #[serde(skip)]
    pub(crate) deferred_kinds: WorkSet,
    /// Next age deadlines observed by this pass.
    #[serde(skip)]
    pub(crate) raw_due: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub(crate) managed_due: Option<DateTime<Utc>>,
}
impl CollectionReport {
    fn defer(&mut self, work: Work) {
        self.deferred = true;
        self.deferred_kinds.insert(work);
    }
}
/// Where an age-only raw pass resumes, when its next deadline is due, and
/// whether the next pass must start from the oldest execution again.
#[derive(Default)]
struct RawCursor {
    after: Option<(String, i64)>,
    due: Option<DateTime<Utc>>,
    rescan: bool,
}
#[derive(Debug, Serialize)]
pub struct StorageDetails {
    #[serde(flatten)]
    pub status: StorageStatus,
    pub database_reusable_bytes: u64,
    pub logical_history_bytes: u64,
    pub logical_raw_bytes: u64,
    pub logical_index_bytes: u64,
    pub logical_managed_bytes: u64,
    pub warning: bool,
    pub limit_reached: bool,
    pub reclaimable_is_lower_bound: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct HistoryStep {
    pub stage: String,
    pub root_revision: i64,
    pub scanned: u64,
    pub nodes_tombstoned: u64,
    pub bytes_collected: u64,
    pub restarted: bool,
    pub complete: bool,
    /// The step was deferred at the DB step budget and rolled back.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
}
#[derive(Debug, Serialize, Deserialize)]
struct Progress {
    stage: Stage,
    last_rowid: i64,
    horizon: i64,
    revalidate_roots: bool,
    #[serde(default)]
    receipt_horizon: i64,
}
#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Receipts,
    Roots,
    Provenance,
    Sweep,
}
impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Receipts => "receipts",
            Self::Roots => "roots",
            Self::Provenance => "provenance",
            Self::Sweep => "sweep",
        }
    }
}

#[derive(Clone)]
struct RawExecution {
    id: String,
    ended: Option<String>,
    started: String,
    rowid: i64,
    order_key: String,
    marked: bool,
    pinned: bool,
    bytes: u64,
}

impl GcService {
    pub fn new(
        database: DbWorker,
        config: Arc<Config>,
        data_dir: PathBuf,
        attachments: Arc<AttachmentService>,
    ) -> Self {
        Self {
            requester: Arc::new(Mutex::new(BackgroundRequester::new(database.clone()))),
            raw_cursor: Arc::default(),
            event_cursor: Arc::default(),
            database,
            files: BlockingFiles::new(config.workers.control_file),
            config,
            data_dir,
            attachments,
            availability_changed: Arc::new(Notify::new()),
        }
    }
    pub(crate) async fn availability_changed(&self) {
        self.availability_changed.notified().await;
    }
    pub async fn wait_idle(&self) -> Result<()> {
        self.files.wait_idle().await
    }
    pub async fn status(&self) -> Result<StorageStatus> {
        Ok(self.details().await?.status)
    }
    /// `storage status`: read on a reader WAL connection, not the writer
    /// (storage design #171). Logical totals come from the schema22 counters;
    /// the raw split is an on-request scan off the writer worker.
    pub async fn details(&self) -> Result<StorageDetails> {
        let path = self.database.path().to_path_buf();
        let raw_cutoff =
            cutoff(Utc::now(), self.config.retention.tool_raw_days).map(|v| v.to_rfc3339());
        let (logical, raw) = crate::presentation::work(move || -> Result<_> {
            let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(sql_error)?;
            db.busy_timeout(std::time::Duration::from_millis(250))
                .map_err(sql_error)?;
            db.pragma_update(None, "query_only", true)
                .map_err(sql_error)?;
            let tx = db.unchecked_transaction().map_err(sql_error)?;
            let logical = logical_usage(&tx)?;
            let raw = raw_stats_tx(&tx, raw_cutoff)?;
            Ok((logical, raw))
        })
        .await??;
        let reclaimable = raw
            .3
            .max(raw.0.saturating_sub(self.config.retention.tool_raw_bytes))
            .min(raw.2);
        let directory = self.data_dir.clone();
        let physical = self
            .files
            .execute(
                tokio::time::Instant::now()
                    + Duration::from_millis(self.config.io.file_prepare_timeout_ms),
                CancellationToken::new(),
                move || physical_usage(&directory),
            )
            .await?;
        let total = logical.total();
        Ok(StorageDetails {
            status: StorageStatus {
                logical_live_bytes: total,
                reclaimable_bytes: reclaimable,
                db_bytes: physical.0,
                wal_bytes: physical.1,
                managed_bytes: physical.2,
                log_bytes: physical.3,
                filesystem_free_bytes: physical.4,
            },
            database_reusable_bytes: logical.reusable,
            logical_history_bytes: logical.history,
            logical_raw_bytes: logical.raw,
            logical_index_bytes: logical.index,
            logical_managed_bytes: logical.managed,
            warning: total >= self.config.limits.data_warning_bytes,
            limit_reached: total >= self.config.limits.data_max_bytes,
            reclaimable_is_lower_bound: true,
        })
    }
    /// Call before new model/shell/child/index/summary work, not before stopping,
    /// terminal records, observation, or collection itself.
    pub async fn require_capacity(&self) -> Result<()> {
        let config = self.config.clone();
        let result = self
            .database
            .call(move |db| Ok(require_capacity_tx(db, &config)))
            .await
            .map_err(crate::app::storage_error)?;
        if matches!(&result, Err(error) if error.code == ErrorCode::StorageLimit) {
            // Backstop for a missed producer signal: the cap itself marks
            // every storage kind, so storage cannot grow past it unnoticed.
            self.database.signals().mark_all(&GC_WORK);
        }
        result
    }
    /// Exclusive use of the GC background requester: tests and callers that
    /// step history GC by hand hold it so maintenance cannot interleave.
    pub async fn hold(&self) -> GcHold<'_> {
        GcHold(self.requester.lock().await)
    }
    /// Explicit `storage collect`: run the category's kinds from the start.
    pub async fn collect(
        &self,
        category: StorageCategory,
        at: DateTime<Utc>,
    ) -> Result<CollectionReport> {
        let works: &[Work] = match category {
            StorageCategory::All => &GC_WORK,
            StorageCategory::Raw => &[Work::Raw],
            StorageCategory::History => &[
                Work::EndedRuns,
                Work::EventBacklog,
                Work::Drafts,
                Work::History,
                Work::SummaryCache,
            ],
            StorageCategory::Index => &[Work::IndexSources, Work::InactiveSpaces],
            StorageCategory::ManagedImages => &[Work::Managed],
        };
        self.raw_cursor.lock().expect("raw cursor").rescan = true;
        let mut hold = self.hold().await;
        self.collect_held(&mut hold, works, at).await
    }
    /// Maintenance requester: only the signalled kinds (storage design #171).
    pub(crate) async fn collect_kinds(
        &self,
        set: WorkSet,
        at: DateTime<Utc>,
    ) -> Result<CollectionReport> {
        let works: Vec<Work> = GC_WORK.into_iter().filter(|w| set.contains(*w)).collect();
        let mut hold = self.hold().await;
        self.collect_held(&mut hold, &works, at).await
    }
    async fn collect_held(
        &self,
        hold: &mut GcHold<'_>,
        works: &[Work],
        at: DateTime<Utc>,
    ) -> Result<CollectionReport> {
        let requester = &mut *hold.0;
        let mut report = CollectionReport::default();
        let has = |work| works.contains(&work);
        if has(Work::Raw) {
            self.collect_raw(requester, at, &mut report).await?;
        }
        if has(Work::Drafts) {
            loop {
                let unit = requester
                    .unit(UNIT, |db, _| {
                        let tx = db.transaction()?;
                        let removed = crate::control::submission_retention::collect_batch(&tx)?;
                        tx.commit()?;
                        Ok(removed as u64)
                    })
                    .await
                    .map_err(crate::app::storage_error)?;
                let Background::Done(removed) = unit else {
                    report.defer(Work::Drafts);
                    break;
                };
                report.submitted_drafts_collected += removed;
                if removed < BATCH as u64 {
                    break;
                }
            }
        }
        if has(Work::EndedRuns) {
            self.collect_ended_runs(requester, &mut report).await?;
        }
        if has(Work::EventBacklog) {
            self.collect_event_backlog(requester, &mut report).await?;
        }
        if has(Work::History) {
            match self.start_history_held(requester).await? {
                Background::Deferred => report.defer(Work::History),
                Background::Done(None) => {}
                Background::Done(Some(job)) => {
                    let mut restarts = 0;
                    loop {
                        let step = self.history_step_held(requester, &job).await?;
                        if step.deferred {
                            report.defer(Work::History);
                            break;
                        }
                        report.nodes_tombstoned += step.nodes_tombstoned;
                        report.history_bytes_collected += step.bytes_collected;
                        if step.complete {
                            break;
                        }
                        if step.restarted {
                            restarts += 1;
                            if restarts >= 3 {
                                report.deferred = true;
                                break;
                            }
                        }
                    }
                }
            }
        }
        if has(Work::SummaryCache) {
            let mut after = String::from("summary.part.");
            loop {
                let cursor = after.clone();
                let unit = requester
                    .unit(UNIT, move |db, limit| {
                        let tx = db.transaction()?;
                        // A successful part is reusable only for exact source revisions.
                        // Once any source was explicitly deletion-GC'd it cannot match.
                        let rows = tx
                            .prepare(SUMMARY_PAGE)?
                            .query_map(params![cursor, limit.get()], |r| {
                                Ok((
                                    r.get::<_, String>(0)?,
                                    r.get::<_, i64>(1)? as u64,
                                    r.get::<_, bool>(2)?,
                                ))
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        let mut collected = (0u64, 0u64);
                        for (key, bytes, stale) in &rows {
                            if *stale {
                                tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
                                collected.0 += 1;
                                collected.1 += bytes;
                            }
                        }
                        tx.commit()?;
                        let last = (rows.len() == limit.rows())
                            .then(|| rows.last().map(|r| r.0.clone()))
                            .flatten();
                        Ok((collected, last))
                    })
                    .await
                    .map_err(crate::app::storage_error)?;
                let Background::Done(((count, bytes), last)) = unit else {
                    report.defer(Work::SummaryCache);
                    break;
                };
                report.summary_cache_parts_collected += count;
                report.summary_cache_bytes_collected += bytes;
                match last {
                    Some(last) => after = last,
                    None => break,
                }
            }
        }
        if has(Work::IndexSources) {
            let mut after = String::new();
            loop {
                let cursor = after.clone();
                let unit = requester
                    .unit(UNIT, move |db, limit| {
                        let tx = db.transaction()?;
                        let rows = tx
                            .prepare(INDEX_SOURCES_PAGE)?
                            .query_map(params![cursor, limit.get()], |row| {
                                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        let mut removed = 0u64;
                        for (node, owns) in &rows {
                            if !owns {
                                continue;
                            }
                            if let Err(error) = crate::index::delete_source_vectors_tx(&tx, node) {
                                return Ok(Err(error));
                            }
                            removed += 1;
                        }
                        tx.commit()?;
                        let last = (rows.len() == limit.rows())
                            .then(|| rows.last().map(|r| r.0.clone()))
                            .flatten();
                        Ok(Ok((removed, last)))
                    })
                    .await
                    .map_err(crate::app::storage_error)?;
                let Background::Done(outcome) = unit else {
                    report.defer(Work::IndexSources);
                    break;
                };
                let (removed, last) = outcome?;
                report.index_sources_collected += removed;
                match last {
                    Some(last) => after = last,
                    None => break,
                }
            }
        }
        if has(Work::InactiveSpaces) {
            loop {
                let unit = requester
                    .unit(SPACE_UNIT, |db, _| {
                        crate::index::collect_inactive_spaces(db)
                    })
                    .await
                    .map_err(crate::app::storage_error)?;
                let Background::Done(removed) = unit else {
                    report.defer(Work::InactiveSpaces);
                    break;
                };
                report.inactive_index_spaces_collected += removed;
                if removed < 32 {
                    break;
                }
            }
        }
        if has(Work::Managed) {
            let managed = self
                .attachments
                .collect_managed_observed(at, Some(self.availability_changed.clone()))
                .await?;
            if managed.deferred {
                report.defer(Work::Managed);
            }
            report.managed_due = managed.next_due;
            report.managed_deleted_count = managed.deleted.len() as u64;
            report.managed_failure_count = managed.failed.len() as u64;
            report.managed_details_truncated =
                managed.deleted.len() > 128 || managed.failed.len() > 128;
            report.managed_deleted = managed.deleted.into_iter().take(128).collect();
            report.managed_failures = managed
                .failed
                .into_iter()
                .take(128)
                .map(|(id, reason)| {
                    let bounded: String = reason.chars().take(512).collect();
                    if bounded.len() < reason.len() {
                        report.managed_details_truncated = true;
                    }
                    (id, bounded)
                })
                .collect();
            report.managed_protected_bytes = managed.protected_bytes;
        }
        Ok(report)
    }
    /// Targeted retention (#170) for each ended Run queued by `run_ended`:
    /// only that Run's pending deliveries and shell events, by index seek.
    async fn collect_ended_runs(
        &self,
        requester: &mut BackgroundRequester,
        report: &mut CollectionReport,
    ) -> Result<()> {
        let signals = self.database.signals().clone();
        let mut runs = signals.take_ended_runs().into_iter();
        while let Some(run) = runs.next() {
            let mut cursor = event_retention::RunCursor::default();
            while cursor != event_retention::RunCursor::Done {
                let id = run.clone();
                let unit = requester
                    .unit(UNIT, move |db, _| {
                        event_retention::collect_run(db, &id, cursor)
                    })
                    .await
                    .map_err(crate::app::storage_error)?;
                let Background::Done(next) = unit else {
                    // Resume this Run (from its start: units are idempotent).
                    signals.requeue_ended_runs(std::iter::once(run).chain(runs).collect());
                    report.defer(Work::EndedRuns);
                    return Ok(());
                };
                cursor = next;
            }
        }
        Ok(())
    }
    /// The full round-robin sweep for rows that predate this owner: only on
    /// startup, explicit `storage collect`, or an ended-Run queue overflow.
    async fn collect_event_backlog(
        &self,
        requester: &mut BackgroundRequester,
        report: &mut CollectionReport,
    ) -> Result<()> {
        // One sweep ends once each scan has wrapped at least once; the two
        // scans wrap independently, so never wait for both at once.
        let (mut deliveries_done, mut events_done) = (false, false);
        loop {
            let cursor = *self.event_cursor.lock().expect("event cursor");
            let unit = requester
                .unit(UNIT, move |db, _| {
                    event_retention::collect_batch(db, cursor)
                })
                .await
                .map_err(crate::app::storage_error)?;
            let Background::Done(next) = unit else {
                report.defer(Work::EventBacklog);
                return Ok(());
            };
            *self.event_cursor.lock().expect("event cursor") = next;
            let (deliveries, events) = next.wrapped();
            deliveries_done |= deliveries;
            events_done |= events;
            if deliveries_done && events_done {
                return Ok(());
            }
        }
    }
    /// Raw retention in bounded windows over `shell_terminal_time`. The total
    /// comes from the schema22 counter. Without a capacity excess, a due age
    /// deadline, pending tombstones or an explicit rescan, the pass is idle
    /// without touching any execution row.
    async fn collect_raw(
        &self,
        requester: &mut BackgroundRequester,
        at: DateTime<Utc>,
        report: &mut CollectionReport,
    ) -> Result<()> {
        let cap = self.config.retention.tool_raw_bytes;
        let cutoff = cutoff(at, self.config.retention.tool_raw_days);
        let unit = requester
            .unit(ONE_ROW, |db, _| {
                let total = usage::raw_total(db).map_err(StorageError::from)?;
                let marked: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key>='raw_tombstone.' AND key<'raw_tombstone/' AND json_extract(value,'$.state')='deleting')",
                    [],
                    |r| r.get(0),
                )?;
                Ok((total, marked))
            })
            .await
            .map_err(crate::app::storage_error)?;
        let Background::Done((mut total, marked)) = unit else {
            report.defer(Work::Raw);
            return Ok(());
        };
        let (mut after, rescan) = {
            let mut cursor = self.raw_cursor.lock().expect("raw cursor");
            let due = cursor.due.is_some_and(|due| due <= at);
            let rescan = std::mem::take(&mut cursor.rescan) || marked;
            if !rescan && !due && total <= cap {
                report.raw_due = cursor.due;
                return Ok(());
            }
            if rescan || total > cap {
                cursor.after = None;
            }
            (cursor.after.clone().unwrap_or_default(), rescan)
        };
        let full = after == (String::new(), 0);
        let mut eligible_bytes = 0u64;
        let mut next_due: Option<DateTime<Utc>> = None;
        let mut resume: Option<(String, i64)> = None;
        loop {
            let cursor = after.clone();
            let unit = requester
                .unit(RAW_UNIT, move |db, limit| {
                    Ok(db
                        .prepare(RAW_PAGE)?
                        .query_map(params![cursor.0, cursor.1, limit.get()], |row| {
                            Ok(RawExecution {
                                id: row.get(0)?,
                                ended: row.get(2)?,
                                started: row.get(3)?,
                                rowid: row.get(4)?,
                                order_key: row.get(5)?,
                                marked: row.get(6)?,
                                pinned: row.get(7)?,
                                bytes: row.get::<_, i64>(8)? as u64,
                            })
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?)
                })
                .await
                .map_err(crate::app::storage_error)?;
            let Background::Done(window) = unit else {
                report.defer(Work::Raw);
                break;
            };
            let more = window.len() == RAW_UNIT.get() as usize;
            for candidate in window {
                after = (candidate.order_key.clone(), candidate.rowid);
                if candidate.pinned || candidate.bytes == 0 && !candidate.marked {
                    continue;
                }
                eligible_bytes += candidate.bytes;
                let ended = DateTime::parse_from_rfc3339(
                    candidate.ended.as_deref().unwrap_or(&candidate.started),
                )
                .ok()
                .map(|ended| ended.with_timezone(&Utc));
                let expired = cutoff.is_some_and(|cutoff| ended.is_some_and(|e| e <= cutoff));
                if !candidate.marked && !expired && total <= cap {
                    // Oldest retained output: its age deadline, and where an
                    // age-only pass resumes next time.
                    if next_due.is_none() {
                        next_due = ended.and_then(|e| {
                            i64::try_from(self.config.retention.tool_raw_days)
                                .ok()
                                .and_then(chrono::Duration::try_days)
                                .and_then(|days| e.checked_add_signed(days))
                        });
                        resume = Some((candidate.order_key.clone(), candidate.rowid - 1));
                    }
                    continue;
                }
                let reason = if expired {
                    "raw_retention_age"
                } else {
                    "raw_retention_capacity"
                };
                let (execution, at, reason) =
                    (candidate.id.clone(), at.to_rfc3339(), reason.to_owned());
                let started = requester
                    .unit(ONE_ROW, move |db, _| {
                        Ok(begin_raw(db, &execution, &reason, &at))
                    })
                    .await
                    .map_err(crate::app::storage_error)?;
                let started = match started {
                    Background::Done(started) => started?,
                    Background::Deferred => {
                        report.defer(Work::Raw);
                        return Ok(());
                    }
                };
                if !started {
                    report.deferred = true;
                    continue;
                }
                if !candidate.marked {
                    // begin_raw has committed its new retention generation.
                    self.availability_changed.notify_one();
                }
                loop {
                    let execution = candidate.id.clone();
                    let step = requester
                        .unit(UNIT, move |db, _| Ok(raw_step(db, &execution)))
                        .await
                        .map_err(crate::app::storage_error)?;
                    let step = match step {
                        Background::Done(step) => step?,
                        Background::Deferred => {
                            report.defer(Work::Raw);
                            return Ok(());
                        }
                    };
                    if step.0 > 0 || (step.1 && !step.2) {
                        self.availability_changed.notify_one();
                    }
                    report.raw_bytes_collected += step.0;
                    total = total.saturating_sub(step.0);
                    if step.1 {
                        break;
                    }
                    if step.2 {
                        report.deferred = true;
                        break;
                    }
                }
            }
            if !more {
                break;
            }
        }
        if full && !report.deferred_kinds.contains(Work::Raw) {
            // Only a complete pass from the start sees every eligible byte.
            report.protected_raw_bytes =
                (total + report.raw_bytes_collected).saturating_sub(eligible_bytes);
        }
        if total > cap {
            report.deferred = true;
        }
        let mut cursor = self.raw_cursor.lock().expect("raw cursor");
        cursor.due = next_due;
        cursor.after = resume;
        cursor.rescan |= rescan && report.deferred_kinds.contains(Work::Raw);
        report.raw_due = next_due;
        Ok(())
    }
    /// Start/restart marking for an explicit Thread-deletion GC job. The TEMP
    /// marks are disposable; durable progress is never trusted without them.
    pub async fn start_history(&self) -> Result<Option<String>> {
        let mut hold = self.hold().await;
        match self.start_history_held(&mut hold.0).await? {
            Background::Done(job) => Ok(job),
            Background::Deferred => Ok(None),
        }
    }
    pub async fn history_step(&self, job_id: &str) -> Result<HistoryStep> {
        let mut hold = self.hold().await;
        self.history_step_held(&mut hold.0, job_id).await
    }
    /// The held variants let a caller step a job without maintenance interleaving.
    pub async fn start_history_with(&self, hold: &mut GcHold<'_>) -> Result<Option<String>> {
        match self.start_history_held(&mut hold.0).await? {
            Background::Done(job) => Ok(job),
            Background::Deferred => Ok(None),
        }
    }
    pub async fn history_step_with(
        &self,
        hold: &mut GcHold<'_>,
        job_id: &str,
    ) -> Result<HistoryStep> {
        self.history_step_held(&mut hold.0, job_id).await
    }
    async fn start_history_held(
        &self,
        requester: &mut BackgroundRequester,
    ) -> Result<Background<Option<String>>> {
        match requester
            .unit(UNIT, |db, _| Ok(start_history(db)))
            .await
            .map_err(crate::app::storage_error)?
        {
            Background::Done(job) => Ok(Background::Done(job?)),
            Background::Deferred => Ok(Background::Deferred),
        }
    }
    async fn history_step_held(
        &self,
        requester: &mut BackgroundRequester,
        job_id: &str,
    ) -> Result<HistoryStep> {
        let job = job_id.to_owned();
        let step = match requester
            .unit(UNIT, move |db, _| Ok(history_step(db, &job)))
            .await
            .map_err(crate::app::storage_error)?
        {
            Background::Done(step) => step?,
            Background::Deferred => HistoryStep {
                stage: "deferred".into(),
                root_revision: 0,
                scanned: 0,
                nodes_tombstoned: 0,
                bytes_collected: 0,
                restarted: false,
                complete: false,
                deferred: true,
            },
        };
        if step.nodes_tombstoned > 0 {
            self.availability_changed.notify_one();
        }
        Ok(step)
    }
}

/// Exclusive hold of the GC background requester.
pub struct GcHold<'a>(tokio::sync::MutexGuard<'a, BackgroundRequester>);

/// Garbage-collection kinds, in the order one pass runs them.
const GC_WORK: [Work; 9] = [
    Work::Raw,
    Work::EndedRuns,
    Work::EventBacklog,
    Work::Drafts,
    Work::History,
    Work::SummaryCache,
    Work::IndexSources,
    Work::InactiveSpaces,
    Work::Managed,
];
const UNIT: NonZeroU32 = NonZeroU32::new(BATCH as u32).unwrap();
const ONE_ROW: NonZeroU32 = NonZeroU32::new(1).unwrap();
const SPACE_UNIT: NonZeroU32 = NonZeroU32::new(32).unwrap();
/// Executions examined per raw window; each also sums its own chunks.
const RAW_UNIT: NonZeroU32 = NonZeroU32::new(32).unwrap();

/// One window of terminal executions in retention order (`shell_terminal_time`).
const RAW_PAGE: &str = "SELECT s.id,s.state,s.ended_at,s.started_at,s.rowid,COALESCE(s.ended_at,s.started_at),EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||s.id AND json_extract(value,'$.state')='deleting'),EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id=s.id),COALESCE((SELECT SUM(length(bytes)) FROM output_chunk WHERE execution_id=s.id),0) FROM shell_execution s INDEXED BY shell_terminal_time WHERE s.state IN ('exited','failed','stopped') AND COALESCE(s.ended_at,s.started_at)>=?1 AND (COALESCE(s.ended_at,s.started_at)>?1 OR s.rowid>?2) ORDER BY COALESCE(s.ended_at,s.started_at),s.rowid LIMIT ?3";
/// One key-range page of summary parts with their staleness.
const SUMMARY_PAGE: &str = "SELECT cache.key,length(CAST(cache.value AS BLOB)),EXISTS(SELECT 1 FROM json_each(CASE WHEN json_valid(cache.value) THEN cache.value ELSE '{}' END,'$.sources') s LEFT JOIN node n ON n.id=s.value WHERE n.id IS NULL OR (n.payload_json IS NULL AND n.deleted_reason IS NOT NULL)) FROM app_meta cache WHERE cache.key>?1 AND cache.key<'summary.part/' ORDER BY cache.key LIMIT ?2";
/// One keyset page of payload-less/deleted nodes (`nodes_index_gc`) with
/// whether each still owns index rows; ownership probes are seeks (#164).
const INDEX_SOURCES_PAGE: &str = "SELECT n.id,EXISTS(SELECT 1 FROM embedding_job WHERE source_node_id=n.id) OR EXISTS(SELECT 1 FROM embedding_chunk WHERE source_node_id=n.id) FROM node n INDEXED BY nodes_index_gc WHERE (n.payload_json IS NULL OR n.deleted_reason IS NOT NULL) AND n.id>?1 ORDER BY n.id LIMIT ?2";

/// Full raw split (total, protected, eligible, reclaimable by age/marker).
/// Runs only on a reader connection for `storage status`.
fn raw_stats_tx(db: &Connection, cutoff: Option<String>) -> Result<(u64, u64, u64, u64)> {
    db.query_row("WITH sizes AS (SELECT s.*,COALESCE((SELECT SUM(length(bytes)) FROM output_chunk WHERE execution_id=s.id),0) AS size,EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id=s.id) AS pinned,EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||s.id) AS marked FROM shell_execution s), eligible AS(SELECT *,state IN ('exited','failed','stopped') AND NOT pinned AS eligible FROM sizes) SELECT COALESCE(SUM(size),0),COALESCE(SUM(CASE WHEN NOT eligible THEN size ELSE 0 END),0),COALESCE(SUM(CASE WHEN eligible THEN size ELSE 0 END),0),COALESCE(SUM(CASE WHEN eligible AND (marked OR julianday(COALESCE(ended_at,started_at))<=julianday(?1)) THEN size ELSE 0 END),0) FROM eligible",[cutoff],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64,r.get::<_,i64>(2)? as u64,r.get::<_,i64>(3)? as u64))).map_err(sql_error)
}

/// Shared DB-lane gate for model, shell, child, summary and index workers.
/// Stop, terminal-record, observation and GC operations must remain available.
pub fn require_capacity_tx(db: &Connection, config: &Config) -> Result<()> {
    let used = logical_usage(db)?;
    if used.total() >= config.limits.data_max_bytes {
        return Err(ApiError::new(
            ErrorCode::StorageLimit,
            "Logical data limit reached; new work is held",
        )
        .hint("Collect eligible raw/managed data or free capacity, then explicitly retry")
        .details(
            json!({"logical_live_bytes":used.total(),"limit_bytes":config.limits.data_max_bytes}),
        ));
    }
    Ok(())
}

fn terminal(state: &str) -> bool {
    matches!(state, "exited" | "failed" | "stopped")
}
fn cutoff(at: DateTime<Utc>, days: u64) -> Option<DateTime<Utc>> {
    i64::try_from(days)
        .ok()
        .and_then(chrono::Duration::try_days)
        .and_then(|duration| at.checked_sub_signed(duration))
}
fn begin_raw(db: &mut Connection, execution: &str, reason: &str, at: &str) -> Result<bool> {
    let tx = db.transaction().map_err(sql_error)?;
    let row=tx.query_row("SELECT state,retention_generation,EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id=?1) FROM shell_execution WHERE id=?1",[execution],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,bool>(2)?))).optional().map_err(sql_error)?;
    let Some((state, generation, pinned)) = row else {
        return Ok(false);
    };
    if pinned || !terminal(&state) {
        return Ok(false);
    }
    let marker = format!("{RAW_PREFIX}{execution}");
    let marked: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
            [&marker],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !marked {
        let nodes=tx.prepare("SELECT DISTINCT r.node_id FROM tool_result r JOIN tool_call c ON c.group_id=r.group_id AND c.call_id=r.call_id WHERE c.operation_id=?1 OR c.operation_id=(SELECT operation_id FROM shell_execution WHERE id=?1)").map_err(sql_error)?.query_map([execution],|row|row.get::<_,String>(0)).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
        for node in nodes {
            crate::index::delete_raw_vectors_tx(&tx, &node)?;
        }
        tx.execute(
            "UPDATE shell_execution SET retention_generation=retention_generation+1 WHERE id=?1",
            [execution],
        )
        .map_err(sql_error)?;
        tx.execute("INSERT INTO app_meta(key,value)VALUES(?1,?2)",params![marker,json!({"reason":reason,"state":"deleting","collected_at":at,"generation":generation+1}).to_string()]).map_err(sql_error)?;
    }
    tx.commit().map_err(sql_error)?;
    Ok(true)
}
/// Returns (deleted bytes, complete, deferred by a newly observed pin).
fn raw_step(db: &mut Connection, execution: &str) -> Result<(u64, bool, bool)> {
    let tx = db.transaction().map_err(sql_error)?;
    let pinned:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id=?1)",[execution],|row|row.get(0)).map_err(sql_error)?;
    if pinned {
        return Ok((0, false, true));
    }
    let chunks=tx.prepare("SELECT sequence,length(bytes) FROM output_chunk WHERE execution_id=?1 ORDER BY sequence LIMIT ?2").map_err(sql_error)?.query_map(params![execution,BATCH as i64],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)? as u64))).map_err(sql_error)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_error)?;
    let mut bytes = 0;
    for (sequence, size) in chunks {
        if bytes > 0 && bytes + size > RAW_BATCH_BYTES {
            break;
        }
        tx.execute(
            "DELETE FROM output_chunk WHERE execution_id=?1 AND sequence=?2",
            params![execution, sequence],
        )
        .map_err(sql_error)?;
        bytes += size;
    }
    let remaining: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM output_chunk WHERE execution_id=?1)",
            [execution],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !remaining {
        tx.execute(
            "UPDATE app_meta SET value=json_set(value,'$.state','deleted') WHERE key=?1",
            [format!("{RAW_PREFIX}{execution}")],
        )
        .map_err(sql_error)?;
    }
    tx.commit().map_err(sql_error)?;
    Ok((bytes, !remaining, false))
}

fn work_tables(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS shellboy_gc_session(job_id TEXT PRIMARY KEY); CREATE TEMP TABLE IF NOT EXISTS shellboy_gc_mark(job_id TEXT NOT NULL,node_id TEXT NOT NULL,expanded INTEGER NOT NULL DEFAULT 0,next_source_rowid INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(job_id,node_id)); CREATE INDEX IF NOT EXISTS temp.shellboy_gc_unexpanded ON shellboy_gc_mark(job_id,expanded,node_id);").map_err(sql_error)
}
fn revision(db: &Connection) -> Result<i64> {
    let value: String = db
        .query_row(
            "SELECT value FROM app_meta WHERE key='root_revision'",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    value
        .parse()
        .map_err(|_| error("root revision metadata is malformed"))
}
fn reset(tx: &Transaction<'_>, job: &str, root: i64) -> Result<()> {
    tx.execute("DELETE FROM shellboy_gc_mark WHERE job_id=?1", [job])
        .map_err(sql_error)?;
    tx.execute(
        "INSERT OR IGNORE INTO shellboy_gc_session VALUES(?1)",
        [job],
    )
    .map_err(sql_error)?;
    let horizon: i64 = tx
        .query_row("SELECT COALESCE(MAX(rowid),0) FROM node", [], |row| {
            row.get(0)
        })
        .map_err(sql_error)?;
    tx.execute("UPDATE gc_job SET root_revision=?2,cursor_json=?3,state='running',error_json=NULL WHERE id=?1",params![job,root,serde_json::to_string(&Progress{stage:Stage::Receipts,last_rowid:0,horizon,revalidate_roots:false,receipt_horizon:tx.query_row("SELECT COALESCE(MAX(rowid),0) FROM receipt",[],|r|r.get(0)).map_err(sql_error)?}).unwrap()]).map_err(sql_error)?;
    Ok(())
}
fn start_history(db: &mut Connection) -> Result<Option<String>> {
    work_tables(db)?;
    let job:Option<String>=db.query_row("SELECT id FROM gc_job WHERE kind='history' AND state IN ('pending','running') ORDER BY rowid LIMIT 1",[],|row|row.get(0)).optional().map_err(sql_error)?;
    if let Some(job) = &job {
        let tx = db.transaction().map_err(sql_error)?;
        let cursor: Option<String> = tx.query_row(
            "SELECT cursor_json FROM gc_job WHERE id=?1 AND state='running' AND EXISTS(SELECT 1 FROM shellboy_gc_session WHERE job_id=?1)",
            [job], |row| row.get(0),
        ).optional().map_err(sql_error)?.flatten();
        let resumable = cursor
            .and_then(|body| serde_json::from_str::<Progress>(&body).ok())
            .is_some();
        if !resumable {
            let root = revision(&tx)?;
            reset(&tx, job, root)?;
        }
        tx.commit().map_err(sql_error)?;
    }
    Ok(job)
}
use crate::history::reachability::ROOT_PREDICATE;

// Ownership evidence is needed only while a payload can appear in search.
// Also called for old tombstones encountered by the ordinary finite sweep.
fn release_deleted_ownership(db: &Connection, node: &str) -> Result<()> {
    db.execute("DELETE FROM thread_path WHERE node_id=?1 AND thread_id IN (SELECT id FROM thread WHERE deleted_at IS NOT NULL) AND EXISTS(SELECT 1 FROM node WHERE id=?1 AND payload_json IS NULL)",[node]).map_err(sql_error)?;
    db.execute("DELETE FROM thread_replacement WHERE replacement_id IN (SELECT id FROM replacement WHERE output_node_id=?1) AND thread_id IN (SELECT id FROM thread WHERE deleted_at IS NOT NULL) AND EXISTS(SELECT 1 FROM node WHERE id=?1 AND payload_json IS NULL)",[node]).map_err(sql_error)?;
    Ok(())
}

fn mark(tx: &Transaction<'_>, job: &str, node: &str) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO shellboy_gc_mark(job_id,node_id)VALUES(?1,?2)",
        params![job, node],
    )
    .map_err(sql_error)?;
    Ok(())
}

fn error(message: impl Into<String>) -> ApiError {
    crate::storage::save_error(message)
}
fn sql_error(error: rusqlite::Error) -> ApiError {
    self::error(format!("collection database operation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{INDEX_SOURCES_PAGE, RAW_PAGE, logical_usage, raw_stats_tx};
    use rusqlite::{Connection, params};
    use std::time::Instant;

    fn fixture(live: usize, deleted: usize, jobs: usize, chunks: usize) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        tx.execute("INSERT INTO index_space(id,profile_id,model_id,dimension,splitter_version,state,vec_table_name)VALUES('s','p','m',4,'v','active','vec_s')",[]).unwrap();
        let payload = format!("{{\"text\":\"{}\"}}", "x".repeat(1000));
        for i in 0..live {
            tx.execute("INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES(?1,'user','2026',?2,?3)",params![format!("n{i:06}"),payload,payload.len() as i64]).unwrap();
        }
        for i in 0..deleted {
            tx.execute(
                "INSERT INTO node(id,kind,created_at,deleted_reason)VALUES(?1,'user','2026','gc')",
                [format!("d{i:06}")],
            )
            .unwrap();
        }
        // Index rows belong to live nodes only, like the measured DB: the
        // query returns zero rows and must still stay bounded.
        for i in 0..jobs {
            tx.execute("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state)VALUES(?1,'s',?2,0,'text','h',?3,?3,'done')",params![format!("j{i:07}"),format!("n{:06}",i%live.max(1)),i as i64]).unwrap();
        }
        for i in 0..chunks {
            tx.execute("INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES(?1,?2,'s',?3,0,0,0,'h',?4)",params![format!("c{i:07}"),format!("j{i:07}"),format!("n{:06}",i%live.max(1)),i as i64]).unwrap();
        }
        tx.commit().unwrap();
        db
    }

    fn plan(db: &Connection) -> Vec<String> {
        db.prepare(&format!("EXPLAIN QUERY PLAN {INDEX_SOURCES_PAGE}"))
            .unwrap()
            .query_map(params!["", 128], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Every owning source across bounded keyset pages.
    fn sources(db: &Connection) -> Vec<String> {
        let (mut owned, mut after) = (Vec::new(), String::new());
        loop {
            let page: Vec<(String, bool)> = db
                .prepare(INDEX_SOURCES_PAGE)
                .unwrap()
                .query_map(params![after, 128], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            owned.extend(page.iter().filter(|r| r.1).map(|r| r.0.clone()));
            match page.last() {
                Some(last) if page.len() == 128 => after = last.0.clone(),
                _ => return owned,
            }
        }
    }

    #[test]
    fn index_source_scan_seeks_deleted_nodes_and_owned_rows() {
        let db = fixture(4, 3, 8, 4);
        db.execute_batch("INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state)VALUES('jd','s','d000001',0,'text','h',0,0,'done'); INSERT INTO embedding_chunk(id,job_id,space_id,source_node_id,source_revision,span_start,span_end,content_hash,vec_row_id)VALUES('cd','jd','s','d000002',0,0,0,'h',99); INSERT INTO node(id,kind,created_at,payload_json,payload_bytes)VALUES('m','user','2026',NULL,0); INSERT INTO embedding_job(id,space_id,source_node_id,source_revision,body_kind,source_hash,span_start,span_end,state)VALUES('jm','s','m',0,'text','h',0,0,'done');").unwrap();
        assert_eq!(sources(&db), ["d000001", "d000002", "m"]);
        let plan = plan(&db);
        assert!(
            plan.iter().any(
                |d| (d.starts_with("SCAN n USING") || d.starts_with("SEARCH n USING"))
                    && d.contains("INDEX nodes_index_gc")
            ),
            "{plan:?}"
        );
        assert!(
            plan.iter()
                .any(|d| d
                    .contains("embedding_job USING COVERING INDEX jobs_source (source_node_id=?)")),
            "{plan:?}"
        );
        assert!(
            plan.iter().any(|d| d
                .contains("embedding_chunk USING COVERING INDEX chunks_source (source_node_id=?)")),
            "{plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|d| d.starts_with("SCAN") && !d.contains("nodes_index_gc")),
            "{plan:?}"
        );
        assert!(!plan.iter().any(|d| d.contains("TEMP B-TREE")), "{plan:?}");
    }

    /// Synthetic size of the measured stall (#164). Prints timings only.
    /// `cargo test --lib gc::tests::index_source_scan_measure -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn index_source_scan_measure() {
        let db = fixture(58_902, 5_098, 130_000, 44_000);
        let time = |label: &str| {
            let started = Instant::now();
            let rows = sources(&db).len();
            println!(
                "{label}: {rows} rows in {:?}; plan {:?}",
                started.elapsed(),
                plan(&db)
            );
        };
        time("after (jobs_source + nodes_index_gc)");
        db.execute_batch("DROP INDEX nodes_index_gc").unwrap();
        time("jobs_source only");
        db.execute_batch("DROP INDEX jobs_source").unwrap();
        time("before (no new indexes)");
    }

    #[test]
    #[ignore = "opt-in read-only StorageCollect raw-page projection audit"]
    fn measure_storage_collect_raw_page_projection_audit() {
        use rusqlite::{
            OpenFlags, StatementStatus,
            trace::{TraceEvent, TraceEventCodes},
        };
        use std::sync::{Mutex, OnceLock};
        static TRACE: OnceLock<Mutex<Vec<(String, std::time::Duration, i32)>>> = OnceLock::new();
        let path = std::path::PathBuf::from(
            std::env::var_os("WRITER_AUDIT_DB").expect("verified disposable DB copy required"),
        );
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open verified copy read-only");
        db.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT | TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(|event| {
                if let TraceEvent::Profile(stmt, elapsed) = event {
                    TRACE.get_or_init(Default::default).lock().unwrap().push((
                        stmt.sql().into_owned(),
                        elapsed,
                        stmt.get_status(StatementStatus::FullscanStep),
                    ));
                }
            }),
        );
        let mut timings = Vec::new();
        for repeat in 1..=4 {
            TRACE.get_or_init(Default::default).lock().unwrap().clear();
            let start = Instant::now();
            let _ = super::usage::raw_total(&db).expect("production raw total projection");
            let mut statement = db
                .prepare(RAW_PAGE)
                .expect("prepare production collection page");
            let _ids = statement
                .query_map(params!["", 0i64, 32i64], |row| row.get::<_, String>(0))
                .expect("run production collection page")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("collect bounded page");
            let elapsed_us = start.elapsed().as_micros();
            if repeat > 1 {
                timings.push(elapsed_us);
            }
            eprintln!(
                "AUDIT kind=StorageCollectRawProjection repeat={repeat} elapsed_us={elapsed_us}"
            );
            let statements = { TRACE.get_or_init(Default::default).lock().unwrap().clone() };
            for (sql, elapsed, fullscan) in statements {
                eprintln!(
                    "AUDIT_SQL kind=StorageCollectRawProjection elapsed_us={} fullscan_step={} sql={}",
                    elapsed.as_micros(),
                    fullscan,
                    sql
                );
                if let Ok(mut plan) = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")) {
                    let values =
                        std::iter::repeat_n(rusqlite::types::Value::Null, plan.parameter_count());
                    if let Ok(rows) = plan.query_map(rusqlite::params_from_iter(values), |row| {
                        row.get::<_, String>(3)
                    }) {
                        for detail in rows.flatten() {
                            eprintln!(
                                "AUDIT_PLAN kind=StorageCollectRawProjection detail={detail}"
                            );
                        }
                    }
                }
            }
        }
        eprintln!("AUDIT_TIMES kind=StorageCollectRawProjection values_us={timings:?}");
    }

    static STORAGE_AUDIT_TRACE: std::sync::OnceLock<
        std::sync::Mutex<Vec<(String, std::time::Duration, i32)>>,
    > = std::sync::OnceLock::new();

    #[test]
    #[ignore = "opt-in StorageStatus DB projections on a verified copy; physical filesystem scan excluded"]
    fn measure_storage_status_db_audit() {
        use rusqlite::{
            OpenFlags, StatementStatus,
            trace::{TraceEvent, TraceEventCodes},
        };
        let path = std::path::PathBuf::from(
            std::env::var_os("WRITER_AUDIT_DB").expect("verified disposable DB copy required"),
        );
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open verified copy read-only");
        db.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT | TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(|event| {
                if let TraceEvent::Profile(stmt, elapsed) = event {
                    STORAGE_AUDIT_TRACE
                        .get_or_init(Default::default)
                        .lock()
                        .unwrap()
                        .push((
                            stmt.sql().into_owned(),
                            elapsed,
                            stmt.get_status(StatementStatus::FullscanStep),
                        ));
                }
            }),
        );
        let mut timings: Vec<u128> = Vec::new();
        for repeat in 1..=3 {
            STORAGE_AUDIT_TRACE
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .clear();
            let start = Instant::now();
            let _logical = logical_usage(&db).expect("production StorageStatus logical projection");
            let _raw =
                raw_stats_tx(&db, None).expect("production StorageStatus raw-output projection");
            let elapsed_us = start.elapsed().as_micros();
            timings.push(elapsed_us);
            println!(
                "AUDIT kind=StorageStatusDbProjections repeat={repeat} elapsed_us={elapsed_us}"
            );
            let statements = STORAGE_AUDIT_TRACE
                .get_or_init(Default::default)
                .lock()
                .unwrap()
                .clone();
            for (sql, elapsed, fullscan) in statements {
                println!(
                    "AUDIT_SQL kind=StorageStatusDbProjections elapsed_us={} fullscan_step={} sql={}",
                    elapsed.as_micros(),
                    fullscan,
                    sql
                );
                let explain = format!("EXPLAIN QUERY PLAN {sql}");
                if let Ok(mut stmt) = db.prepare(&explain) {
                    let params =
                        std::iter::repeat_n(rusqlite::types::Value::Null, stmt.parameter_count());
                    if let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                        row.get::<_, String>(3)
                    }) {
                        for detail in rows.flatten() {
                            println!("AUDIT_PLAN kind=StorageStatusDbProjections detail={detail}");
                        }
                    }
                }
            }
        }
        println!("AUDIT_TIMES kind=StorageStatusDbProjections values_us={timings:?}");
    }
}
