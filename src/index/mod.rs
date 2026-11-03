//! Authorized, versioned embedding jobs and transactional sqlite-vec adoption.
//! The source database is authoritative; no historical backfill is implicit.
use crate::digest::sha256_hex as hash;
use crate::{
    config::Config,
    control::output::ShellOutputReader,
    history,
    model::ModelError,
    protocol::{
        ApiError, ErrorCode, HistorySearchArgs, IndexArgs, IndexScope, IndexStatus, NodeProvenance,
        SearchMatch,
    },
    storage::{DbWorker, StorageError},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
mod coverage;
mod queue;
pub(crate) mod saved_view;
mod search;
mod source;
mod space_gc;
mod split;
pub(crate) use space_gc::collect_inactive_spaces;
mod store;
use coverage::{covered_ranges, range_covered};
use queue::{AutoAdmission, Work, bootstrap_key, job_id_for, queue_job};
pub(crate) use search::{rag_hits, rag_profile_key, rag_space};
use source::{Source, source_bodies};
use split::drain_segments;
pub use split::{Segment, split_text};
pub(crate) use store::delete_raw_vectors_tx;
pub use store::delete_source_vectors_tx;
use store::{
    RawWitness, Space, Witness, decode_json, encode_json, group_current, json_meta, load_space,
    maybe_activate_candidate, meta, set_json_meta, set_meta, space_for_group, update_json_meta,
    valid_table, witness_valid,
};
mod batch;
mod claim;
mod failure;
use claim::{claim, claim_valid, discard_claim};
mod dimensions;
mod identity;
mod keys;
mod observations;
mod pause;
pub(crate) use keys::IndexKeys;
#[cfg(test)]
use store::ensure_space;

const SPLITTER: &str = "unicode-paragraph-v1";
const DELETE_BOOTSTRAP_SOURCE: &str = "DELETE FROM app_meta INDEXED BY index_bootstrap_source WHERE key LIKE 'index.bootstrap.%' AND json_valid(value) AND json_extract(value,'$.witness.node_id')=?1";
const DELETE_AUTO_SOURCE: &str = "DELETE FROM app_meta INDEXED BY index_auto_source WHERE key LIKE 'index.auto.%' AND json_valid(value) AND json_extract(value,'$.node_id')=?1";
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn err(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}
fn db_error(error: StorageError) -> ApiError {
    error.into_api_error()
}
fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(error.to_string())
}
fn serde_error() -> StorageError {
    StorageError::Worker("Stored index job is malformed".into())
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FeePreview {
    pub sources: u64,
    pub segments: u64,
    pub characters: u64,
    pub already_indexed: u64,
    pub unavailable: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceSpan {
    pub source_revision: i64,
    pub content_hash: String,
    pub body_kind: String,
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchThreadScope {
    pub thread_id: String,
    pub on_current_path: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexHit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_scope: Option<SearchThreadScope>,
    pub provenance: NodeProvenance,
    pub node_id: String,
    pub excerpt: String,
    pub distance: f64,
    pub spans: Vec<SourceSpan>,
}
#[derive(Clone, Debug)]
struct Claimed {
    requested_dimensions: Option<usize>,
    job_id: Option<String>,
    bootstrap_key: Option<String>,
    space: Option<Space>,
    work: Work,
}
enum ClaimResult {
    Idle,
    Progress,
    Ready(Vec<Claimed>),
}
#[derive(Clone, Copy, Serialize)]
struct StorageDiagnosticStop {
    processing_error_code: ErrorCode,
    pause_error_code: ErrorCode,
}
pub struct IndexService {
    clients: Arc<crate::model::http::HttpClients>,
    database: DbWorker,
    config: Arc<Config>,
    notify: Arc<Notify>,
    saved_changed: Arc<Notify>,
    pause_changed: Arc<Notify>,
    profile_key: Option<String>,
    endpoint_family_key: Option<String>,
    started: AtomicBool,
    storage_diagnostic_stop: OnceLock<StorageDiagnosticStop>,
    diagnostics: OnceLock<crate::diagnostics::Diagnostics>,
    reader: ShellOutputReader,
    source_slots: Arc<Semaphore>,
    search_slots: Arc<Semaphore>,
    stop: CancellationToken,
    /// The index worker's background requester: one outstanding scheduling
    /// unit across workers (storage design #171). Settling claims whose HTTP
    /// request already ran stays on the foreground lane.
    background: tokio::sync::Mutex<crate::storage::BackgroundRequester>,
}
/// Declared bound of one index scheduling unit (single-row or batch claims).
const SCHEDULE_UNIT: std::num::NonZeroU32 = std::num::NonZeroU32::new(128).unwrap();
/// `discard_paused` consumes one paused item per transaction.
const DISCARD_UNIT: std::num::NonZeroU32 = std::num::NonZeroU32::new(1).unwrap();
pub struct IndexWorkers {
    cancel: CancellationToken,
    joins: Vec<JoinHandle<()>>,
}
impl IndexWorkers {
    pub async fn shutdown(self) {
        self.cancel.cancel();
        for join in self.joins {
            let _ = join.await;
        }
    }
}
/// A fixed classification of the persisted pause, never provider prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseReason {
    Transport,
    Authentication,
    Configuration,
    Dimension,
    Capacity,
    Other,
}

// Match the existing saved-observation metadata bound, including normal provider errors.
const MAX_DISPLAY_PAUSE_BYTES: usize = 128 * 1024;

fn pause_reason(raw: &str) -> PauseReason {
    if raw.len() > MAX_DISPLAY_PAUSE_BYTES {
        return PauseReason::Other;
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return PauseReason::Other;
    };
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    let code = value.get("code").and_then(Value::as_str).unwrap_or("");
    let mut error = ModelError::new(kind, "");
    error.provider_code = value
        .get("provider_code")
        .and_then(Value::as_str)
        .map(str::to_owned);
    error.status = value
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok());
    let failure = error.embedding_failure();
    if kind == "dimension_mismatch" {
        PauseReason::Dimension
    } else if failure.credentials_or_credit {
        PauseReason::Authentication
    } else if failure.invalid_configuration {
        PauseReason::Configuration
    } else if code == "storage_limit" || failure.rate_limited {
        PauseReason::Capacity
    } else if failure.transient
        && (matches!(kind, "transport" | "timeout") || error.status == Some(408))
    {
        PauseReason::Transport
    } else {
        PauseReason::Other
    }
}

fn read_display_pause_tx(
    db: &Connection,
    profile: Option<&str>,
    family: Option<&str>,
) -> Result<Option<PauseReason>, StorageError> {
    if profile.is_none() && family.is_none() {
        return Ok(None);
    }
    for scope in family.into_iter().chain(profile) {
        let pause_key = IndexKeys::new(scope).pause();
        let size: Option<i64> = db
            .query_row(
                "SELECT length(CAST(value AS BLOB)) FROM app_meta WHERE key=?1",
                [&pause_key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(size) = size else { continue };
        // Keep malformed oversized saved values bounded, but do not hide the marker.
        if size > MAX_DISPLAY_PAUSE_BYTES as i64 {
            return Ok(Some(PauseReason::Other));
        }
        if let Some(value) = meta(db, &pause_key)? {
            return Ok(Some(pause_reason(&value)));
        }
        return Ok(None);
    }
    Ok(None)
}

impl IndexService {
    /// Read the saved pause fact in the caller's snapshot.
    pub(crate) fn read_display_pause(
        &self,
        db: &Connection,
    ) -> Result<Option<PauseReason>, StorageError> {
        read_display_pause_tx(
            db,
            self.profile_key.as_deref(),
            self.endpoint_family_key.as_deref(),
        )
    }

    pub fn new(database: DbWorker, config: Arc<Config>) -> Arc<Self> {
        Self::with_instance(database, config, format!("index-{}", id()))
    }
    pub fn with_instance(database: DbWorker, config: Arc<Config>, instance: String) -> Arc<Self> {
        let clients = Arc::new(crate::model::http::HttpClients::new(&config));
        Self::with_clients(database, config, instance, clients)
    }
    pub(crate) fn with_clients(
        database: DbWorker,
        config: Arc<Config>,
        instance: String,
        clients: Arc<crate::model::http::HttpClients>,
    ) -> Arc<Self> {
        let profile_key = identity::profile_key(&config);
        let endpoint_family_key = identity::endpoint_family_key(&config);
        Arc::new(Self {
            clients,
            reader: ShellOutputReader::new(database.clone(), instance),
            background: tokio::sync::Mutex::new(crate::storage::BackgroundRequester::new(
                database.clone(),
            )),
            source_slots: Arc::new(Semaphore::new(config.indexing.concurrency.max(1))),
            search_slots: Arc::new(Semaphore::new(1)),
            database,
            config,
            notify: Arc::new(Notify::new()),
            saved_changed: Arc::new(Notify::new()),
            pause_changed: Arc::new(Notify::new()),
            profile_key,
            endpoint_family_key,
            started: AtomicBool::new(false),
            storage_diagnostic_stop: OnceLock::new(),
            diagnostics: OnceLock::new(),
            stop: CancellationToken::new(),
        })
    }
    /// Call after cancelling indexing and draining admitted requests. Acquiring all
    /// permits also waits for detached spawn_blocking source parsers to finish.
    pub async fn wait_idle(&self) -> Result<(), ApiError> {
        let permits = u32::try_from(self.config.indexing.concurrency.max(1)).map_err(|_| {
            err(
                ErrorCode::ConfigInvalid,
                "Index source concurrency exceeds semaphore permit range",
            )
        })?;
        let held = self
            .source_slots
            .clone()
            .acquire_many_owned(permits)
            .await
            .map_err(|_| err(ErrorCode::InvalidState, "Index source workers closed"))?;
        drop(held);
        let search = self
            .search_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| err(ErrorCode::InvalidState, "Index search admission closed"))?;
        drop(search);
        Ok(())
    }
    fn key(&self) -> Result<&str, ApiError> {
        self.profile_key.as_deref().ok_or_else(||err(ErrorCode::ConfigInvalid,"Embedding connection/model is not configured; conversation continues without indexing"))
    }
    /// Fatal owner failures cancel HTTP/job work synchronously. Joining remains
    /// the orderly Control shutdown's responsibility.
    pub(crate) fn cancel(&self) {
        self.stop.cancel();
        self.notify.notify_waiters();
    }
    fn storage_stop_reason(&self) -> Option<String> {
        self.storage_diagnostic_stop.get().map(|stop| {
            let processing = json!(stop.processing_error_code);
            let pause = json!(stop.pause_error_code);
            format!(
                "Index workers stopped after a storage failure; the diagnostic pause could not be saved (processing={}, pause={}). Repair storage and restart the owner",
                processing.as_str().expect("ErrorCode serializes as a string"),
                pause.as_str().expect("ErrorCode serializes as a string"))
        })
    }
    fn stopped_search_error(&self) -> ApiError {
        if let Some(reason) = self.storage_stop_reason() {
            let mut error = err(ErrorCode::InvalidState, &reason);
            error.details =
                Some(json!({ "storage_diagnostic_stop": self.storage_diagnostic_stop.get() }));
            error.hint = Some(
                "Repair storage and restart the owner before issuing another semantic search"
                    .into(),
            );
            error
        } else {
            err(ErrorCode::InvalidState, "Index owner stopped the search")
        }
    }
    pub async fn start(self: &Arc<Self>) -> Result<IndexWorkers, ApiError> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(err(
                ErrorCode::InvalidState,
                "Index workers have already started",
            ));
        }
        // Remove the obsolete #164 cursor and counter; neither tracks work now.
        self.database.call(|db| {
            db.execute("DELETE FROM app_meta WHERE key IN ('index.pause_cleanup.state','index.pause_cleanup.discarded')", [])?;
            Ok(())
        }).await.map_err(db_error)?;
        // Inflight calls from a prior owner may already have incurred billing.
        // Keep them failed for explicit retry rather than silently replaying.
        let mut cursor = None;
        loop {
            let (changed, next, done) = self
                .database
                .call(move |db| queue::recover_inflight_page(db, cursor))
                .await
                .map_err(db_error)?;
            self.publish_saved(changed);
            if done {
                break;
            }
            cursor = next;
        }
        let mut cursor = None;
        loop {
            let (result, changed) = self.database
                .call(move |db| Ok(queue::recover_authorization_page(db, cursor)))
                .await
                .map_err(db_error)?;
            self.publish_saved(changed);
            let (next, done) = result.map_err(db_error)?;
            if done { break; }
            cursor = next;
        }
        let cancel = self.stop.child_token();
        let mut joins = Vec::new();
        for _ in 0..self.config.indexing.concurrency {
            let service = self.clone();
            let token = cancel.clone();
            joins.push(tokio::spawn(service.run_worker(token)));
        }
        Ok(IndexWorkers { cancel, joins })
    }
    /// Owner diagnostics for bounded numeric index observations.
    pub(crate) fn set_diagnostics(&self, diagnostics: crate::diagnostics::Diagnostics) {
        let _ = self.diagnostics.set(diagnostics);
    }
    async fn run_worker(self: Arc<Self>, token: CancellationToken) {
        while !token.is_cancelled() {
            match self.process_one(token.clone()).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(_) => {} // The recorded pause makes later consumers discard pending work.
            }
            // No work without progress (#171): sleep only until a scheduled
            // retry; with nothing scheduled or while paused, wait for a wake
            // (enqueue, authorization, retry, select_space) instead of a timer.
            let delay = self
                .next_delay()
                .await
                .unwrap_or(Some(Duration::from_secs(30)));
            tokio::select! {
                _ = token.cancelled() => break,
                _ = self.notify.notified() => {},
                _ = async {
                    match delay {
                        Some(delay) => tokio::time::sleep(delay).await,
                        None => std::future::pending().await,
                    }
                } => {},
            }
        }
    }
    /// Durable, cheap source authorization in the finalization transaction.
    /// This records identifiers only; the background worker performs parsing,
    /// raw decoding and segmentation without delaying conversation execution.
    pub fn authorize_new_tx(
        &self,
        tx: &Transaction<'_>,
        node_ids: &[String],
    ) -> Result<(), ApiError> {
        if !self.config.indexing.auto_history {
            return Ok(());
        }
        let Some(key) = &self.profile_key else {
            return Ok(());
        };
        for node_id in node_ids {
            let value = serde_json::to_string(&AutoAdmission {
                recovery: false,
                recovery_group: None,
                recovery_authorization: None,
                recovery_witness: None,
                recovery_body_kind: None,
                node_id: node_id.clone(),
                state: "pending".into(),
                error: None,
            })
            .map_err(|_| err(ErrorCode::InvalidState, "Cannot encode index authorization"))?;
            tx.execute(
                "INSERT OR IGNORE INTO app_meta(key,value)VALUES(?1,?2)",
                params![IndexKeys::new(key).auto(node_id), value],
            )
            .map_err(sql_error)?;
        }
        Ok(())
    }
    /// Saved index changes only; never consume the worker wake permit.
    pub(crate) async fn saved_changed(&self) {
        self.saved_changed.notified().await;
    }
    pub(crate) async fn pause_changed(&self) {
        self.pause_changed.notified().await;
    }
    fn publish_pause(&self, changed: bool) {
        if changed {
            self.pause_changed.notify_one();
        }
    }
    fn publish_saved(&self, changed: bool) {
        if changed {
            self.saved_changed.notify_one();
        }
    }
    pub fn wake(&self) {
        self.notify.notify_waiters();
        self.notify.notify_one();
    }
    /// Runs each worker scheduling unit once (per-kind step test, #171).
    #[cfg(test)]
    pub(crate) async fn measure_background_units(&self) {
        let _ = self.process_one(CancellationToken::new()).await;
        let _ = self.next_delay().await;
    }
    /// Records a profile pause directly (per-kind step test, #171).
    #[cfg(test)]
    pub(crate) async fn pause_for_tests(&self) {
        let key = IndexKeys::new(self.profile_key.as_deref().expect("profile")).pause();
        self.database
            .call(move |db| {
                db.execute(
                    "INSERT INTO app_meta(key,value)VALUES(?1,'{\"reason\":\"step test\"}')",
                    [key],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    /// Space-state transitions make inactive spaces collectable (#171).
    pub(super) fn spaces_changed(&self) {
        self.database
            .signals()
            .mark(crate::maintenance::Work::InactiveSpaces);
    }
    /// One declared bounded background unit; `None` when it was deferred at
    /// the DB step budget (rolled back; the worker retries it next round).
    async fn background_unit<T, F>(
        &self,
        limit: std::num::NonZeroU32,
        operation: F,
    ) -> Result<Option<T>, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&mut rusqlite::Connection, crate::storage::Limit) -> Result<T, StorageError>
            + Send
            + 'static,
    {
        match self
            .background
            .lock()
            .await
            .unit(limit, operation)
            .await
            .map_err(db_error)?
        {
            crate::storage::Background::Done(value) => Ok(Some(value)),
            crate::storage::Background::Deferred => Ok(None),
        }
    }
    async fn drain_authorization(&self, key: &str) -> Result<bool, ApiError> {
        let auto_history = self.config.indexing.auto_history;
        let range = IndexKeys::new(key).auto_range();
        let profile = key.to_owned();
        let family = self.failure_key().map(str::to_owned);
        let Some((discarded, admission)) = self
            .background_unit(SCHEDULE_UNIT, move |db, _| {
                queue::claim_authorization(db, &range, &profile, family.as_deref(), auto_history)
            })
            .await?
        else {
            return Ok(true); // Deferred: nothing committed; retry next round.
        };
        self.publish_saved(discarded);
        let Some((record, mut admission)) = admission else {
            return Ok(discarded);
        };
        self.publish_saved(true); // claim_authorization committed pending -> running
        let authorization = admission
            .recovery_authorization
            .as_deref()
            .unwrap_or("new-finalized");
        let admission_group = admission.recovery_group.as_deref().unwrap_or("active");
        match self
            .enqueue(
                &[admission.node_id.clone()],
                admission_group,
                authorization,
                admission
                    .recovery_witness
                    .as_ref()
                    .zip(admission.recovery_body_kind.as_deref()),
            )
            .await
        {
            Ok(_) => {
                let lookup = IndexKeys::new(key).rebuild();
                let group = self
                    .database
                    .call(move |db| meta(db, &lookup))
                    .await
                    .map_err(db_error)?;
                if !admission.recovery
                    && let Some(group) = &group
                    && let Err(error) = self
                        .enqueue(
                            std::slice::from_ref(&admission.node_id),
                            group,
                            "new-finalized-during-rebuild",
                            None,
                        )
                        .await
                {
                    admission.state = "failed".into();
                    admission.error = Some(error.to_string());
                    let changed = self
                        .database
                        .call(move |db| Ok(update_json_meta(db, &record, &admission)? > 0))
                        .await
                        .map_err(db_error)?;
                    self.publish_saved(changed);
                    return Ok(true);
                }
                let profile = key.to_owned();
                let changed = self
                    .database
                    .call(move |db| {
                        let tx = db.transaction()?;
                        let mut changed =
                            tx.execute("DELETE FROM app_meta WHERE key=?1", [record])? > 0;
                        if let Some(group) = group {
                            changed |=
                                maybe_activate_candidate(&tx, IndexKeys::new(&profile), &group)?;
                        }
                        tx.commit()?;
                        Ok(changed)
                    })
                    .await
                    .map_err(db_error)?;
                self.publish_saved(changed);
                if changed {
                    self.spaces_changed(); // a candidate activation retires the old space
                }
            }
            Err(error) => {
                admission.state = "failed".into();
                admission.error = Some(error.to_string());
                let changed = self
                    .database
                    .call(move |db| Ok(update_json_meta(db, &record, &admission)? > 0))
                    .await
                    .map_err(db_error)?;
                self.publish_saved(changed);
            }
        }
        Ok(true)
    }
    pub async fn enqueue_new(&self, node_ids: &[String]) -> Result<FeePreview, ApiError> {
        if self.profile_key.is_none() || !self.config.indexing.auto_history {
            return Ok(FeePreview::default());
        }
        self.enqueue(node_ids, "active", "new-finalized", None)
            .await
    }
    /// A named convenience for the collector's finalized full-raw source.
    pub async fn enqueue_tool_raw(&self, node_id: &str) -> Result<FeePreview, ApiError> {
        self.enqueue_new(&[node_id.to_owned()]).await
    }
    pub async fn preview(&self, args: IndexArgs) -> Result<FeePreview, ApiError> {
        if args.scope.is_none() && args.source_ids.is_empty() && args.thread_ids.is_empty() {
            return self.retry_preview(args).await;
        }
        let ids = self.selected_sources(&args).await?;
        let mut result = FeePreview::default();
        let write_space = self.write_space(&args).await?;
        for node in ids {
            if let Some(source) = self.load_source(&node).await? {
                result.sources += 1;
                let space = write_space.clone();
                let jobs = self
                    .coverage_jobs(&source, space)
                    .await?
                    .into_iter()
                    .filter(|job| {
                        job.witness
                            .as_ref()
                            .is_some_and(|witness| witness.matches_body(&source.witness, &job.kind))
                    })
                    .collect::<Vec<_>>();
                for (kind, body) in source_bodies(&source) {
                    result.characters += body.chars().count() as u64;
                    let segments = split_text(&body, self.config.indexing.split_chars, 0);
                    let ranges = covered_ranges(&kind, &body, 0, &jobs);
                    result.already_indexed += segments
                        .iter()
                        .filter(|segment| range_covered(&ranges, segment))
                        .count() as u64;
                    result.segments += segments.len() as u64;
                }
                if let Some(raw) = &source.witness.raw {
                    let stream = self.reader.stream(&raw.execution).await?;
                    let mut segments =
                        source::RawSegments::new(stream, self.config.indexing.split_chars);
                    while let Some((characters, segments)) = segments.next_page().await? {
                        result.characters += characters;
                        for segment in &segments {
                            let ranges =
                                covered_ranges("tool_raw", &segment.text, segment.start, &jobs);
                            result.already_indexed += u64::from(range_covered(&ranges, segment));
                        }
                        result.segments += segments.len() as u64;
                    }
                }
            } else {
                result.unavailable += 1;
            }
        }
        Ok(result)
    }
    /// Observe global index context at an existing Thread/Turn boundary only.
    /// Event and observation marker are part of the caller's transaction.
    pub fn record_context_observations_tx(
        &self,
        tx: &Transaction<'_>,
        thread: &str,
        at: &str,
    ) -> Result<(), ApiError> {
        let live: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM thread WHERE id=?1 AND deleted_at IS NULL)",
                [thread],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !live {
            return Err(err(
                ErrorCode::ThreadNotFound,
                "Index context target Thread is unavailable",
            ));
        }
        let active = meta(tx, IndexKeys::ACTIVE_SPACE).map_err(db_error)?;
        let space = active
            .as_deref()
            .map(|id| load_space(tx, id))
            .transpose()
            .map_err(db_error)?
            .flatten();
        let profile = space
            .as_ref()
            .map(|s| s.profile.clone())
            .or_else(|| self.config.embedding.connection.clone());
        let model = space
            .as_ref()
            .map(|s| s.model.clone())
            .or_else(|| self.config.embedding.model.clone());
        let mut pause_keys = self.identity_keys();
        if let Some(family) = self.failure_key() {
            pause_keys.insert(0, family.to_owned());
        }
        if let Some(key) = space
            .as_ref()
            .and_then(|s| s.splitter.strip_prefix(&format!("{SPLITTER}:")))
            && !pause_keys.iter().any(|existing| existing == key)
        {
            pause_keys.push(key.to_owned());
        }
        let mut pause = None;
        let mut paused_at = None;
        let mut pause_id = None;
        for key in pause_keys {
            if let Some(value) = meta(tx, &IndexKeys::new(&key).pause()).map_err(db_error)? {
                let (at, id) = pause::facts(&IndexKeys::new(&key).pause(), &value);
                paused_at = at;
                pause_id = Some(id);
                pause = Some(value);
                break;
            }
        }
        let pause_kind = pause.as_ref().map(|text| {
            serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|v| {
                    v.get("kind")
                        .or_else(|| v.get("code"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "unknown".into())
        });
        let current = json!({
        "active_space":active,
        "profile":profile,
        "model":model,
        "dimension":space.as_ref().map(|s|s.dimension),
        "active_state":space.as_ref().map(|s|s.state.as_str()),
        "paused":pause.is_some(), "paused_at":paused_at, "pause_id":pause_id,
        "pause_kind":pause_kind}
        );

        let key = IndexKeys::observed(thread);
        let before = meta(tx, &key)
            .map_err(db_error)?
            .map(|value| serde_json::from_str::<Value>(&value))
            .transpose()
            .map_err(|_| {
                err(
                    ErrorCode::InvalidState,
                    "Stored index context observation is malformed",
                )
            })?;
        if before.as_ref() != Some(&current) {
            crate::control::context::record(
                tx,
                thread,
                "index_state",
                json!({"kind":"shellboy"}),
                json!({"before":before,"after":current}),
                at,
            )
            .map_err(sql_error)?;
            set_meta(tx, &key, &current.to_string()).map_err(db_error)?;
        }
        Ok(())
    }
    pub async fn backfill(&self, args: IndexArgs) -> Result<FeePreview, ApiError> {
        self.key()?;
        self.write_space(&args).await?;
        let ids = self.selected_sources(&args).await?;
        self.enqueue(&ids, "active", "explicit-backfill", None)
            .await
    }
    pub async fn rebuild(&self, args: IndexArgs) -> Result<FeePreview, ApiError> {
        let key = self.key()?.to_owned();
        if args
            .dimensions
            .is_some_and(|n| !(1..=crate::storage::MAX_VECTOR_DIMENSIONS).contains(&n))
        {
            return Err(err(
                ErrorCode::InvalidArgument,
                "dimensions must be in 1..=8192",
            ));
        }
        self.write_space(&args).await?;
        let requested = args.dimensions;
        let full_rebuild = args.scope == Some(IndexScope::All) && args.space_id.is_none();
        let config = self.config.clone();
        let ids = self.selected_sources(&args).await?;
        if ids.is_empty() {
            return Ok(FeePreview::default());
        }
        let group = format!("rebuild-{}", id());
        let group_copy = group.clone();
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                if let Some(previous) = meta(&tx, &IndexKeys::new(&key).rebuild())?
                    && let Some(space) = meta(&tx, &IndexKeys::new(&key).group(&previous))?
                {
                    tx.execute(
                        "UPDATE index_space SET state='inactive' WHERE id=?1 AND state<>'active'",
                        [space],
                    )?;
                }
                let requested = match requested {
                    Some(n) => Some(n),
                    None => {
                        if let Some(space) = meta(&tx, IndexKeys::ACTIVE_SPACE)?
                            .map(|id| load_space(&tx, &id))
                            .transpose()?
                            .flatten()
                        {
                            let marker = meta(&tx, &IndexKeys::query_profile(&space.id))?;
                            if config.embedding.connection.as_deref()
                                == Some(space.profile.as_str())
                                && config.embedding.model.as_deref() == Some(space.model.as_str())
                                && identity::query_profile_key(
                                    &config,
                                    &space.profile,
                                    &space.model,
                                    marker.as_deref().unwrap_or(""),
                                )
                                .is_some()
                            {
                                space.requested_dimensions
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                };
                let mode_key = dimensions::candidate_key(IndexKeys::new(&key));
                if let Some(dimensions) = requested {
                    set_json_meta(
                        &tx,
                        &mode_key,
                        &dimensions::CandidateMode {
                            group: group_copy.clone(),
                            dimensions,
                            error: None,
                            paused_at: None,
                        },
                    )?;
                } else {
                    tx.execute("DELETE FROM app_meta WHERE key=?1", [mode_key])?;
                }
                let keys = IndexKeys::new(&key);
                tx.execute(
                    "DELETE FROM app_meta WHERE key=?1",
                    [keys.recovery_rebuild()],
                )?;
                if full_rebuild && let Some(marker) = meta(&tx, &keys.recovery_required())? {
                    set_json_meta(
                        &tx,
                        &keys.recovery_rebuild(),
                        &json!({"group": group_copy, "marker": marker}),
                    )?;
                }
                set_meta(&tx, &keys.rebuild(), &group_copy)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(true);
        self.spaces_changed(); // the previous rebuild candidate may be inactive now
        self.enqueue(&ids, &group, "explicit-rebuild", None).await
    }
    async fn enqueue(
        &self,
        node_ids: &[String],
        group: &str,
        authorization: &str,
        expected: Option<(&Witness, &str)>,
    ) -> Result<FeePreview, ApiError> {
        let key = self.key()?.to_owned();
        let mut preview = FeePreview::default();
        let mut seen = HashSet::new();
        let barrier_required = group != "active" && !authorization.ends_with("during-rebuild");
        if barrier_required {
            let barrier = IndexKeys::new(&key).enqueueing(group);
            self.database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let count = meta(&tx, &barrier)?
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(0)
                        + 1;
                    set_meta(&tx, &barrier, &count.to_string())?;
                    tx.commit()?;
                    Ok(())
                })
                .await
                .map_err(db_error)?;
            self.publish_saved(true);
        }
        let enqueue_result: Result<(), ApiError> = async {
            for node in node_ids {
                if !seen.insert(node) {
                    continue;
                }
                let Some(source) = self.load_source(node).await? else {
                    preview.unavailable += 1;
                    continue;
                };
                if expected
                    .is_some_and(|(witness, kind)| !source.witness.matches_body(witness, kind))
                {
                    return Err(err(
                        ErrorCode::OperationConflict,
                        "Recovered index source changed; explicitly reauthorize it",
                    ));
                }
                preview.sources += 1;
                for (body_kind, text) in source_bodies(&source) {
                    let segments = split_text(&text, self.config.indexing.split_chars, 0);
                    preview.characters += text.chars().count() as u64;
                    for batch in segments.chunks(128) {
                        let work: Vec<_> = batch
                            .iter()
                            .cloned()
                            .map(|segment| {
                                let mut work = Work::pending(
                                    source.witness.canonical(),
                                    body_kind.clone(),
                                    segment,
                                    group,
                                    authorization,
                                );
                                work.recovery = expected.is_some();
                                work
                            })
                            .collect();
                        let (inserted, total) = self.enqueue_batch(&key, work).await?;
                        preview.segments += inserted as u64;
                        preview.already_indexed += (total - inserted) as u64;
                    }
                }
                if let Some(raw) = &source.witness.raw {
                    let stream = self.reader.stream(&raw.execution).await?;
                    if stream.retention_generation() != raw.generation as u64
                        || stream.saved_sequence() != raw.saved_sequence as u64
                    {
                        return Err(err(
                            ErrorCode::OperationConflict,
                            "Raw output changed while authorizing index work",
                        ));
                    }
                    let mut segments =
                        source::RawSegments::new(stream, self.config.indexing.split_chars);
                    while let Some((characters, segments)) = segments.next_page().await? {
                        preview.characters += characters;
                        let work: Vec<_> = segments
                            .into_iter()
                            .map(|segment| {
                                let mut work = Work::pending(
                                    source.witness.clone(),
                                    "tool_raw".into(),
                                    segment,
                                    group,
                                    authorization,
                                );
                                work.recovery = expected.is_some();
                                work
                            })
                            .collect();
                        for batch in work.chunks(128) {
                            let (inserted, total) =
                                self.enqueue_batch(&key, batch.to_vec()).await?;
                            preview.segments += inserted as u64;
                            preview.already_indexed += (total - inserted) as u64;
                        }
                    }
                }
                if !self.config.indexing.auto_history && !authorization.starts_with("new-finalized")
                {
                    let record = IndexKeys::new(&key).auto(node);
                    self.database
                        .call(move |db| {
                            db.execute("DELETE FROM app_meta WHERE key=?1", [record])?;
                            Ok(())
                        })
                        .await
                        .map_err(db_error)?;
                }
            }
            Ok(())
        }
        .await;
        if barrier_required {
            let key = key.clone();
            let group = group.to_owned();
            let enqueue_ok = enqueue_result.is_ok();
            let failure_recorded = if !enqueue_ok {
                let key = key.clone();
                let group = group.clone();
                self.database
                    .call(move |db| {
                        let tx = db.transaction()?;
                        set_meta(&tx, &IndexKeys::new(&key).enqueue_failed(&group), "1")?;
                        tx.commit()?;
                        Ok(())
                    })
                    .await
                    .map_err(db_error)
            } else {
                Ok(())
            };
            self.publish_saved(!enqueue_ok && failure_recorded.is_ok());
            let release_group = group.clone();
            self.database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let barrier = IndexKeys::new(&key).enqueueing(&release_group);
                    let count = meta(&tx, &barrier)?
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(1);
                    if count <= 1 {
                        tx.execute("DELETE FROM app_meta WHERE key=?1", [barrier])?;
                    } else {
                        set_meta(&tx, &barrier, &(count - 1).to_string())?;
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await
                .map_err(db_error)?;
            self.publish_saved(true);
            // Neither failure recording nor activation can roll back release.
            failure_recorded?;
            let key = self.key()?.to_owned();
            let group = group.clone();
            self.database
                .call(move |db| {
                    let tx = db.transaction()?;
                    if enqueue_ok {
                        maybe_activate_candidate(&tx, IndexKeys::new(&key), &group)?;
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await
                .map_err(db_error)?;
            self.publish_saved(true);
        }
        enqueue_result?;
        self.wake();
        Ok(preview)
    }
    async fn capacity_allowed(&self, key: &str) -> Result<bool, ApiError> {
        let key = key.to_owned();
        let config = self.config.clone();
        let (allowed, changed) = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                match crate::gc::require_capacity_tx(&tx, &config) {
                    Ok(()) => {
                        tx.commit()?;
                        Ok((true, false))
                    }
                    Err(error) if error.code == ErrorCode::StorageLimit => {
                        let changed = pause::save(
                            &tx,
                            &IndexKeys::new(&key).pause(),
                            &serde_json::to_value(&error).map_err(|_| serde_error())?,
                        )?;
                        tx.commit()?;
                        Ok((false, changed))
                    }
                    Err(error) => Err(StorageError::from(error)),
                }
            })
            .await
            .map_err(db_error)?;
        self.publish_saved(changed);
        self.publish_pause(changed);
        Ok(allowed)
    }
    /// Worker scheduling check on the background lane; `None` when deferred.
    async fn capacity_allowed_background(&self, key: &str) -> Result<Option<bool>, ApiError> {
        let key = key.to_owned();
        let config = self.config.clone();
        let Some((allowed, changed)) = self
            .background_unit(SCHEDULE_UNIT, move |db, _| {
                let tx = db.transaction()?;
                match crate::gc::require_capacity_tx(&tx, &config) {
                    Ok(()) => {
                        tx.commit()?;
                        Ok((true, false))
                    }
                    Err(error) if error.code == ErrorCode::StorageLimit => {
                        let changed = pause::save(
                            &tx,
                            &IndexKeys::new(&key).pause(),
                            &serde_json::to_value(&error).map_err(|_| serde_error())?,
                        )?;
                        tx.commit()?;
                        Ok((false, changed))
                    }
                    Err(error) => Err(StorageError::from(error)),
                }
            })
            .await?
        else {
            return Ok(None);
        };
        self.publish_saved(changed);
        Ok(Some(allowed))
    }
    pub async fn process_one(&self, cancel: CancellationToken) -> Result<bool, ApiError> {
        let outcome = self.process_one_inner(cancel).await;
        if let Err(error) = &outcome {
            // A failed adoption must not silently strand a running claim while
            // other inputs continue spending. Preserve the original transaction
            // outcome as unknown; a separate existing pause carries the reason.
            let mut keys = self.identity_keys();
            if let Some(family) = self.failure_key() {
                keys.push(family.to_owned());
            }
            let reason = json!({
                "kind":"index_worker_error",
                "message":"Index processing failed; result adoption may be incomplete. Inspect storage, clear the pause, and explicitly preview/backfill unindexed sources",
                "error":error,
            });
            let paused = self
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let mut changed = false;
                    for key in keys {
                        let pause = IndexKeys::new(&key).pause();
                        if meta(&tx, &pause)?.is_none() {
                            changed |= pause::save(&tx, &pause, &reason)?;
                        }
                    }
                    tx.commit()?;
                    Ok(changed)
                })
                .await;
            if let Ok(changed) = &paused {
                self.publish_saved(*changed);
                self.publish_pause(*changed);
            }
            if let Err(pause_error) = paused {
                // Storage cannot preserve even the diagnostic. Stop spending;
                // restart/repair is required rather than a hidden retry loop.
                let _ = self.storage_diagnostic_stop.set(StorageDiagnosticStop {
                    processing_error_code: error.code,
                    pause_error_code: db_error(pause_error).code,
                });
                self.stop.cancel();
            }
        }
        outcome
    }
    async fn process_one_inner(&self, cancel: CancellationToken) -> Result<bool, ApiError> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        for key in self.identity_keys() {
            if self.process_key(key, cancel.clone()).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn process_key(&self, key: String, cancel: CancellationToken) -> Result<bool, ApiError> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let profile = key.clone();
        let family = self.failure_key().map(str::to_owned);
        // One paused item per bounded background unit (#173, #171).
        let Some(paused) = self
            .background_unit(DISCARD_UNIT, move |db, _| {
                claim::discard_paused(db, &profile, family.as_deref())
            })
            .await?
        else {
            return Ok(true); // Deferred: retry next round.
        };
        if let Some(progress) = paused {
            self.publish_saved(progress);
            return Ok(progress);
        }
        match self.capacity_allowed_background(&key).await? {
            Some(true) => {}
            Some(false) => return Ok(false),
            None => return Ok(true), // Deferred: retry next round.
        }
        if self.drain_authorization(&key).await? {
            return Ok(true);
        }
        let key_copy = key.clone();
        let max_inputs = self.config.embedding.batch_max_inputs;
        let max_chars = self.config.embedding.batch_max_chars;
        let auto_history = self.config.indexing.auto_history;
        let family = self.failure_key().map(str::to_owned);
        let limit = u32::try_from(max_inputs)
            .ok()
            .and_then(std::num::NonZeroU32::new)
            .unwrap_or(SCHEDULE_UNIT);
        let Some(claimed) = self
            .background_unit(limit, move |db, _| {
                claim(
                    db,
                    IndexKeys::new(&key_copy),
                    max_inputs,
                    max_chars,
                    auto_history,
                    family.as_deref(),
                )
            })
            .await?
        else {
            return Ok(true); // Deferred: nothing was claimed; retry next round.
        };
        let claims = match claimed {
            ClaimResult::Idle => return Ok(false),
            ClaimResult::Progress => {
                self.publish_saved(true);
                return Ok(true);
            }
            ClaimResult::Ready(claims) => {
                self.publish_saved(true);
                claims
            }
        };
        self.process_claims(&key, claims, cancel).await?;
        self.wake();
        Ok(true)
    }
}
#[cfg(test)]
mod idle_tests {
    use super::*;
    #[tokio::test]
    async fn owner_stop_rejects_search_before_query_or_http_work() {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("stopped-search.sqlite"))
            .await
            .unwrap();
        let service = IndexService::new(db.clone(), Arc::new(Config::default()));
        service.cancel();
        let error = service.search("synthetic query", 10).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(error.message.contains("owner stopped"));
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn unsaved_diagnostic_pause_reports_storage_stop_in_search_and_readable_status() {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("storage-stop.sqlite"))
            .await
            .unwrap();
        let mut config = Config::default();
        config.connections.insert(
            "fixture".into(),
            crate::config::Connection {
                base_url: "http://127.0.0.1:1/v1".into(),
                ..Default::default()
            },
        );
        config.embedding.connection = Some("fixture".into());
        config.embedding.model = Some("synthetic".into());
        let service = IndexService::new(db.clone(), Arc::new(config));
        let record = IndexKeys::new(service.profile_key.as_deref().unwrap()).auto("malformed");
        let saved_record = record.clone();
        db.call(move |db| {
            // A real authorization-decode failure occurs before any HTTP; a
            // separate injected write failure prevents its diagnostic pause.
            db.execute("INSERT INTO app_meta(key,value)VALUES(?1,'{\"state\":\"pending\"}')",[record])?;
            db.execute_batch("CREATE TRIGGER refuse_index_pause BEFORE INSERT ON app_meta WHEN NEW.key LIKE 'index.pause.%' BEGIN SELECT RAISE(ABORT,'synthetic diagnostic write failure'); END;")?;
            Ok(())
        }).await.unwrap();
        let processing_error = service
            .process_one(CancellationToken::new())
            .await
            .unwrap_err();
        let stopped = service.storage_diagnostic_stop.get().unwrap();
        assert_eq!(stopped.processing_error_code, processing_error.code);
        assert_eq!(stopped.pause_error_code, ErrorCode::SaveFailed);
        let error = service.search("synthetic query", 10).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(
            error
                .message
                .contains("Index workers stopped after a storage failure")
        );
        assert!(!error.message.contains("owner stopped"));
        assert_eq!(
            error.details.as_ref().unwrap()["storage_diagnostic_stop"]["processing_error_code"],
            json!(processing_error.code)
        );
        assert_eq!(
            error.details.as_ref().unwrap()["storage_diagnostic_stop"]["pause_error_code"],
            "save_failed"
        );
        assert!(error.hint.unwrap().contains("restart the owner"));
        db.call(move |db| {
            db.execute_batch("DROP TRIGGER refuse_index_pause")?;
            db.execute("DELETE FROM app_meta WHERE key=?1", [saved_record])?;
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM app_meta WHERE key LIKE 'index.pause.%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
        let pause_key = IndexKeys::new(service.profile_key.as_deref().unwrap()).pause();
        let saved_pause = r#"{"kind":"provider_pause","evidence":"synthetic durable evidence"}"#;
        db.call(move |db| {
            let tx = db.transaction()?;
            set_meta(&tx, &pause_key, saved_pause)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        let status = service.status(IndexArgs::default()).await.unwrap();
        assert!(status.paused);
        let reason = status.reason.unwrap();
        assert!(
            reason.starts_with(saved_pause),
            "durable reason must survive: {reason}"
        );
        assert!(reason.contains("diagnostic pause could not be saved"));
        let details = error.details.as_ref().unwrap();
        for (label, field) in [
            ("processing", "processing_error_code"),
            ("pause", "pause_error_code"),
        ] {
            let code = details["storage_diagnostic_stop"][field].as_str().unwrap();
            assert!(reason.contains(&format!("{label}={code}")), "{reason}");
        }
        assert!(reason.contains("pause=save_failed"));
        assert!(!reason.contains("SaveFailed"));
        // Removing the trigger does not silently restart stopped workers.
        assert!(
            service
                .search("synthetic query", 10)
                .await
                .unwrap_err()
                .message
                .contains("storage failure")
        );
        db.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn wait_idle_retains_detached_blocking_parser_permit_until_actual_completion() {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("index-idle.sqlite"))
            .await
            .unwrap();
        let service = IndexService::new(db.clone(), Arc::new(Config::default()));
        let permit = service.source_slots.clone().acquire_owned().await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, hold) = std::sync::mpsc::channel();
        let detached = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _ = entered.send(());
            hold.recv().unwrap();
        });
        drop(detached);
        ready.await.unwrap();
        let waiting = service.clone();
        let mut idle = tokio::spawn(async move { waiting.wait_idle().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut idle)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), idle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        db.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[tokio::test]
    async fn rejected_embedding_dimensions_preserve_the_existing_space_and_nonfatal_reason() {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("index-errors.sqlite"))
            .await
            .unwrap();
        db.call(|db| {
            let tx = db.transaction()?;
            ensure_space(
                &tx,
                &identity::EmbeddingIdentity {
                    key: "key".into(),
                    profile: "profile".into(),
                    model: "model".into(),
                    query_key: "query-key".into(),
                    requested_dimensions: None,
                },
                "candidate",
                3,
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        let rejected = db
            .call(|db| {
                let tx = db.transaction()?;
                ensure_space(
                    &tx,
                    &identity::EmbeddingIdentity {
                        key: "key".into(),
                        profile: "profile".into(),
                        model: "model".into(),
                        query_key: "query-key".into(),
                        requested_dimensions: None,
                    },
                    "candidate",
                    4,
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap_err()
            .into_api_error();
        assert_eq!(rejected.code, ErrorCode::InvalidState);
        assert!(!rejected.is_storage_failure());
        let invalid = db
            .call(|db| {
                let tx = db.transaction()?;
                ensure_space(
                    &tx,
                    &identity::EmbeddingIdentity {
                        key: "key".into(),
                        profile: "profile".into(),
                        model: "model".into(),
                        query_key: "query-key".into(),
                        requested_dimensions: None,
                    },
                    "new-candidate",
                    0,
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .unwrap_err()
            .into_api_error();
        assert_eq!(invalid.code, ErrorCode::IoFailed);
        let dimensions: Vec<i64> = db
            .call(|db| {
                Ok(db
                    .prepare("SELECT dimension FROM index_space")?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();
        assert_eq!(dimensions, [3]);
        db.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod source_metadata_tests {
    use super::*;
    use rusqlite::StatementStatus;

    #[tokio::test]
    async fn source_cleanup_uses_point_indexes_and_preserves_unidentified_metadata() {
        let app_meta_sql = "SELECT COUNT(*) FROM app_meta WHERE key IN ('index.bootstrap.broken','index.auto.broken','unrelated.broken','unrelated.valid','index.bootstrap.s1.other','index.auto.s1.other')";
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("source-metadata.sqlite"))
            .await
            .unwrap();
        db.call(|db| {
            let tx = db.transaction()?;
            for (key, value) in [
                (
                    "index.bootstrap.s1.target",
                    r#"{"witness":{"node_id":"target"}}"#,
                ),
                (
                    "index.bootstrap.s2.target",
                    r#"{"witness":{"node_id":"target"}}"#,
                ),
                (
                    "INDEX.BOOTSTRAP.s3.target",
                    r#"{"witness":{"node_id":"target"}}"#,
                ),
                ("index.auto.s1.target", r#"{"node_id":"target"}"#),
                ("index.auto.s2.target", r#"{"node_id":"target"}"#),
                ("INDEX.AUTO.s3.target", r#"{"node_id":"target"}"#),
                (
                    "index.bootstrap.s1.other",
                    r#"{"witness":{"node_id":"other"}}"#,
                ),
                ("index.auto.s1.other", r#"{"node_id":"other"}"#),
                ("index.bootstrap.broken", "not JSON"),
                ("index.auto.broken", "not JSON"),
                ("unrelated.broken", "not JSON"),
                (
                    "unrelated.valid",
                    r#"{"witness":{"node_id":"target"},"node_id":"target"}"#,
                ),
            ] {
                tx.execute(
                    "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
                    params![key, value],
                )?;
            }
            for i in 0..10_000 {
                tx.execute(
                    "INSERT INTO app_meta(key,value)VALUES(?1,'unrelated preview bytes')",
                    [format!("index.preview.{i}")],
                )?;
            }
            tx.commit()?;
            for (sql, index) in [
                (DELETE_BOOTSTRAP_SOURCE, "index_bootstrap_source"),
                (DELETE_AUTO_SOURCE, "index_auto_source"),
            ] {
                let mut plan = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let lines = plan
                    .query_map(["target"], |row| row.get::<_, String>(3))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert!(
                    lines
                        .iter()
                        .any(|line| line.contains("SEARCH") && line.contains(index)),
                    "{lines:?}"
                );
                let mut statement = db.prepare(sql)?;
                assert_eq!(statement.execute(["no matching source"])?, 0);
                assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
                assert!(
                    statement.get_status(StatementStatus::VmStep) < 100,
                    "unrelated previews must not be inspected"
                );
            }
            let before: i64 =
                db.query_row("SELECT COUNT(*) FROM app_meta", [], |row| row.get(0))?;
            {
                let tx = db.transaction()?;
                delete_source_vectors_tx(&tx, "target").unwrap();
                assert_eq!(
                    tx.query_row("SELECT COUNT(*) FROM app_meta", [], |row| row
                        .get::<_, i64>(0))?,
                    before - 6
                );
                tx.rollback()?;
            }
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM app_meta", [], |row| row
                    .get::<_, i64>(0))?,
                before
            );
            let tx = db.transaction()?;
            delete_source_vectors_tx(&tx, "target").unwrap();
            tx.commit()?;
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM app_meta", [], |row| row
                    .get::<_, i64>(0))?,
                before - 6
            );
            assert_eq!(
                db.query_row(app_meta_sql, [], |row| row.get::<_, i64>(0))?,
                6
            );
            Ok(())
        })
        .await
        .unwrap();
        db.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod saved_notification_tests;

#[cfg(test)]
mod display_pause_tests {
    use super::*;

    #[test]
    fn absent_configured_scopes_do_not_query_database() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(read_display_pause_tx(&db, None, None).unwrap(), None);
    }

    #[tokio::test]
    async fn real_retry_clears_persisted_pause_and_notifies() {
        let temp = tempfile::tempdir().unwrap();
        let db = DbWorker::open(temp.path().join("pause.sqlite"))
            .await
            .unwrap();
        let mut config = Config::default();
        config.connections.insert(
            "fixture".into(),
            crate::config::Connection {
                base_url: "http://127.0.0.1:1/v1".into(),
                ..Default::default()
            },
        );
        config.embedding.connection = Some("fixture".into());
        config.embedding.model = Some("synthetic".into());
        let mut disabled_config = config.clone();
        disabled_config.indexing.auto_history = false;
        let disabled = IndexService::new(db.clone(), Arc::new(disabled_config));
        assert!(disabled.profile_key.is_some());
        assert!(disabled.endpoint_family_key.is_some());
        let index = disabled.clone();
        assert_eq!(
            db.call(move |db| index.read_display_pause(db))
                .await
                .unwrap(),
            None
        );
        let service = IndexService::new(db.clone(), Arc::new(config));
        let family = service.endpoint_family_key.clone().unwrap();
        let profile = service.profile_key.clone().unwrap();
        db.call(move |db| {
            let tx = db.transaction()?;
            for key in [family, profile] {
                set_meta(
                    &tx,
                    &IndexKeys::new(&key).pause(),
                    r#"{"kind":"transport","message":"private"}"#,
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .unwrap();
        for index in [service.clone(), disabled.clone()] {
            assert_eq!(
                db.call(move |db| index.read_display_pause(db))
                    .await
                    .unwrap(),
                Some(PauseReason::Transport)
            );
        }
        service.retry(IndexArgs::default()).await.unwrap();
        crate::test_watchdog::watchdog("saved pause retry notification", service.pause_changed())
            .await
            .unwrap();
        for index in [service.clone(), disabled] {
            assert_eq!(
                db.call(move |db| index.read_display_pause(db))
                    .await
                    .unwrap(),
                None
            );
        }
        db.shutdown().await.unwrap();
    }

    #[test]
    fn persisted_pause_uses_family_then_profile_and_clears_on_retry() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT INTO app_meta VALUES('index.pause.other','{}');").unwrap();
        assert_eq!(read_display_pause_tx(&db, None, None).unwrap(), None);
        assert_eq!(
            read_display_pause_tx(&db, Some("p"), Some("f")).unwrap(),
            None
        );
        db.execute(
            "INSERT INTO app_meta VALUES('index.pause.p',?1)",
            [r#"{"kind":"transport","message":"private"}"#],
        )
        .unwrap();
        assert_eq!(
            read_display_pause_tx(&db, Some("p"), Some("f")).unwrap(),
            Some(PauseReason::Transport)
        );
        db.execute(
            "INSERT INTO app_meta VALUES('index.pause.f',?1)",
            [r#"{"kind":"provider","status":401}"#],
        )
        .unwrap();
        assert_eq!(
            read_display_pause_tx(&db, Some("p"), Some("f")).unwrap(),
            Some(PauseReason::Authentication)
        );
        assert_eq!(
            read_display_pause_tx(&db, None, Some("f")).unwrap(),
            Some(PauseReason::Authentication)
        );
        db.execute(
            "DELETE FROM app_meta WHERE key IN ('index.pause.p','index.pause.f')",
            [],
        )
        .unwrap();
        assert_eq!(
            read_display_pause_tx(&db, Some("p"), Some("f")).unwrap(),
            None
        );
    }

    #[test]
    fn saved_pause_reason_is_bounded_and_never_displays_raw_text() {
        for (raw, expected) in [
            (r#"{"kind":"transport"}"#, PauseReason::Transport),
            (r#"{"status":403}"#, PauseReason::Authentication),
            (r#"{"kind":"config_missing"}"#, PauseReason::Configuration),
            (r#"{"kind":"dimension_mismatch"}"#, PauseReason::Dimension),
            (r#"{"status":429}"#, PauseReason::Capacity),
            (r#"{"status":500}"#, PauseReason::Other),
            (r#"{"code":"storage_limit"}"#, PauseReason::Capacity),
            (r#"{"message":"private provider text"}"#, PauseReason::Other),
            ("bad json", PauseReason::Other),
        ] {
            assert_eq!(pause_reason(raw), expected);
        }
        let long_error = serde_json::json!({"kind":"provider", "status":401, "message":"x".repeat(crate::model::MAX_ERROR_MESSAGE_CHARS)}).to_string();
        assert_eq!(pause_reason(&long_error), PauseReason::Authentication);
        assert_eq!(
            pause_reason(&"x".repeat(MAX_DISPLAY_PAUSE_BYTES + 1)),
            PauseReason::Other
        );
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE app_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        db.execute(
            "INSERT INTO app_meta VALUES('index.pause.f', ?1)",
            ["x".repeat(1_000_000)],
        )
        .unwrap();
        assert_eq!(
            read_display_pause_tx(&db, Some("p"), Some("f")).unwrap(),
            Some(PauseReason::Other)
        );
    }
}
