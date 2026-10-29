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
        Arc,
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
mod search;
mod source;
mod split;
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
mod claim;
mod failure;
use claim::{claim, claim_valid, discard_claim};
mod identity;
mod keys;
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
    job_id: Option<String>,
    bootstrap_key: Option<String>,
    space: Option<Space>,
    work: Work,
}
enum ClaimResult {
    Idle,
    Progress,
    Ready(Box<Claimed>),
}
pub struct IndexService {
    clients: Arc<crate::model::http::HttpClients>,
    database: DbWorker,
    config: Arc<Config>,
    notify: Arc<Notify>,
    profile_key: Option<String>,
    endpoint_family_key: Option<String>,
    started: AtomicBool,
    storage_diagnostic_failed: AtomicBool,
    reader: ShellOutputReader,
    source_slots: Arc<Semaphore>,
    search_slots: Arc<Semaphore>,
    stop: CancellationToken,
}
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
impl IndexService {
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
            source_slots: Arc::new(Semaphore::new(config.indexing.concurrency.max(1))),
            search_slots: Arc::new(Semaphore::new(1)),
            database,
            config,
            notify: Arc::new(Notify::new()),
            profile_key,
            endpoint_family_key,
            started: AtomicBool::new(false),
            storage_diagnostic_failed: AtomicBool::new(false),
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
    fn storage_stop_reason(&self) -> Option<&'static str> {
        self.storage_diagnostic_failed.load(Ordering::Acquire).then_some("Index workers stopped after a storage failure; the diagnostic pause could not be saved. Repair storage and restart the owner")
    }
    fn stopped_search_error(&self) -> ApiError {
        if let Some(reason) = self.storage_stop_reason() {
            let mut error = err(ErrorCode::InvalidState, reason);
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
        // Inflight calls from a prior owner may already have incurred billing.
        // Keep them failed for explicit retry rather than silently replaying.
        self.database
            .call(queue::recover_inflight)
            .await
            .map_err(db_error)?;
        self.database
            .call(queue::recover_authorization)
            .await
            .map_err(db_error)?;
        let cancel = self.stop.child_token();
        let mut joins = Vec::new();
        for _ in 0..self.config.indexing.concurrency {
            let service = self.clone();
            let token = cancel.clone();
            joins.push(tokio::spawn(service.run_worker(token)));
        }
        Ok(IndexWorkers { cancel, joins })
    }
    async fn run_worker(self: Arc<Self>, token: CancellationToken) {
        while !token.is_cancelled() {
            match self.process_one(token.clone()).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(_) => {} // The recorded pause blocks further claims until explicit retry.
            }
            let delay = self.next_delay().await.unwrap_or(Duration::from_secs(30));
            tokio::select! {
                _ = token.cancelled() => break,
                _ = self.notify.notified() => {},
                _ = tokio::time::sleep(delay) => {},
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
        let Some(key) = &self.profile_key else {
            return Ok(());
        };
        for node_id in node_ids {
            let value = serde_json::to_string(&AutoAdmission {
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
    pub fn wake(&self) {
        self.notify.notify_waiters();
        self.notify.notify_one();
    }
    async fn drain_authorization(&self, key: &str) -> Result<bool, ApiError> {
        let prefix = IndexKeys::new(key).auto_pattern();
        let admission = self
            .database
            .call(move |db| queue::claim_authorization(db, &prefix))
            .await
            .map_err(db_error)?;
        let Some((record, mut admission)) = admission else {
            return Ok(false);
        };
        match self.enqueue_new(&[admission.node_id.clone()]).await {
            Ok(_) => {
                let lookup = IndexKeys::new(key).rebuild();
                let group = self
                    .database
                    .call(move |db| meta(db, &lookup))
                    .await
                    .map_err(db_error)?;
                if let Some(group) = &group
                    && let Err(error) = self
                        .enqueue(
                            std::slice::from_ref(&admission.node_id),
                            group,
                            "new-finalized-during-rebuild",
                        )
                        .await
                {
                    admission.state = "failed".into();
                    admission.error = Some(error.to_string());
                    self.database
                        .call(move |db| {
                            update_json_meta(db, &record, &admission)?;
                            Ok(())
                        })
                        .await
                        .map_err(db_error)?;
                    return Ok(true);
                }
                let profile = key.to_owned();
                self.database
                    .call(move |db| {
                        let tx = db.transaction()?;
                        tx.execute("DELETE FROM app_meta WHERE key=?1", [record])?;
                        if let Some(group) = group {
                            maybe_activate_candidate(&tx, IndexKeys::new(&profile), &group)?;
                        }
                        tx.commit()?;
                        Ok(())
                    })
                    .await
                    .map_err(db_error)?;
            }
            Err(error) => {
                admission.state = "failed".into();
                admission.error = Some(error.to_string());
                self.database
                    .call(move |db| {
                        update_json_meta(db, &record, &admission)?;
                        Ok(())
                    })
                    .await
                    .map_err(db_error)?;
            }
        }
        Ok(true)
    }
    pub async fn enqueue_new(&self, node_ids: &[String]) -> Result<FeePreview, ApiError> {
        if self.profile_key.is_none() {
            return Ok(FeePreview::default());
        }
        self.enqueue(node_ids, "active", "new-finalized").await
    }
    /// A named convenience for the collector's finalized full-raw source.
    pub async fn enqueue_tool_raw(&self, node_id: &str) -> Result<FeePreview, ApiError> {
        self.enqueue_new(&[node_id.to_owned()]).await
    }
    pub async fn preview(&self, args: IndexArgs) -> Result<FeePreview, ApiError> {
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
        if let Some(key) = space
            .as_ref()
            .and_then(|s| s.splitter.strip_prefix(&format!("{SPLITTER}:")))
            && !pause_keys.iter().any(|existing| existing == key)
        {
            pause_keys.push(key.to_owned());
        }
        let mut pause = None;
        for key in pause_keys {
            if let Some(value) = meta(tx, &IndexKeys::new(&key).pause()).map_err(db_error)? {
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
        "paused":pause.is_some(),
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
        self.enqueue(&ids, "active", "explicit-backfill").await
    }
    pub async fn rebuild(&self, args: IndexArgs) -> Result<FeePreview, ApiError> {
        let key = self.key()?.to_owned();
        self.write_space(&args).await?;
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
                set_meta(&tx, &IndexKeys::new(&key).rebuild(), &group_copy)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(db_error)?;
        self.enqueue(&ids, &group, "explicit-rebuild").await
    }
    async fn enqueue(
        &self,
        node_ids: &[String],
        group: &str,
        authorization: &str,
    ) -> Result<FeePreview, ApiError> {
        let key = self.key()?.to_owned();
        let mut preview = FeePreview::default();
        let mut seen = HashSet::new();
        let barrier_required = group != "active" && authorization != "new-finalized-during-rebuild";
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
        }
        for node in node_ids {
            if !seen.insert(node) {
                continue;
            }
            let Some(source) = self.load_source(node).await? else {
                preview.unavailable += 1;
                continue;
            };
            preview.sources += 1;
            for (body_kind, text) in source_bodies(&source) {
                let segments = split_text(&text, self.config.indexing.split_chars, 0);
                preview.characters += text.chars().count() as u64;
                for batch in segments.chunks(128) {
                    let work: Vec<_> = batch
                        .iter()
                        .cloned()
                        .map(|segment| {
                            Work::pending(
                                source.witness.canonical(),
                                body_kind.clone(),
                                segment,
                                group,
                                authorization,
                            )
                        })
                        .collect();
                    let inserted = self.enqueue_batch(&key, work).await?;
                    preview.segments += inserted as u64;
                    preview.already_indexed += (batch.len() - inserted) as u64;
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
                            Work::pending(
                                source.witness.clone(),
                                "tool_raw".into(),
                                segment,
                                group,
                                authorization,
                            )
                        })
                        .collect();
                    for batch in work.chunks(128) {
                        let inserted = self.enqueue_batch(&key, batch.to_vec()).await?;
                        preview.segments += inserted as u64;
                        preview.already_indexed += (batch.len() - inserted) as u64;
                    }
                }
            }
        }
        if barrier_required {
            let key = key.clone();
            let group = group.to_owned();
            self.database
                .call(move |db| {
                    let tx = db.transaction()?;
                    let barrier = IndexKeys::new(&key).enqueueing(&group);
                    let count = meta(&tx, &barrier)?
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(1);
                    if count <= 1 {
                        tx.execute("DELETE FROM app_meta WHERE key=?1", [barrier])?;
                    } else {
                        set_meta(&tx, &barrier, &(count - 1).to_string())?;
                    }
                    maybe_activate_candidate(&tx, IndexKeys::new(&key), &group)?;
                    tx.commit()?;
                    Ok(())
                })
                .await
                .map_err(db_error)?;
        }
        self.wake();
        Ok(preview)
    }
    async fn capacity_allowed(&self, key: &str) -> Result<bool, ApiError> {
        let key = key.to_owned();
        let config = self.config.clone();
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                match crate::gc::require_capacity_tx(&tx, &config) {
                    Ok(()) => {
                        tx.commit()?;
                        Ok(true)
                    }
                    Err(error) if error.code == ErrorCode::StorageLimit => {
                        set_meta(&tx, &IndexKeys::new(&key).pause(), &encode_json(&error)?)?;
                        tx.commit()?;
                        Ok(false)
                    }
                    Err(error) => Err(StorageError::from(error)),
                }
            })
            .await
            .map_err(db_error)
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
                "message":"Index processing failed; result adoption may be incomplete. Inspect storage and retry explicitly",
                "error":error,
            }).to_string();
            let paused = self
                .database
                .call(move |db| {
                    let tx = db.transaction()?;
                    for key in keys {
                        let pause = IndexKeys::new(&key).pause();
                        if meta(&tx, &pause)?.is_none() {
                            set_meta(&tx, &pause, &reason)?;
                        }
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await;
            if paused.is_err() {
                // Storage cannot preserve even the diagnostic. Stop spending;
                // restart/repair is required rather than a hidden retry loop.
                self.storage_diagnostic_failed
                    .store(true, Ordering::Release);
                self.stop.cancel();
            }
        }
        outcome
    }
    async fn process_one_inner(&self, cancel: CancellationToken) -> Result<bool, ApiError> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let Some(family) = self.failure_key().map(str::to_owned) else {
            return Ok(false);
        };
        if self
            .database
            .call(move |db| Ok(meta(db, &IndexKeys::new(&family).pause())?.is_some()))
            .await
            .map_err(db_error)?
        {
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
        if !self.capacity_allowed(&key).await? {
            return Ok(false);
        }
        if self.drain_authorization(&key).await? {
            return Ok(true);
        }
        let key_copy = key.clone();
        let claimed = self
            .database
            .call(move |db| claim(db, IndexKeys::new(&key_copy)))
            .await
            .map_err(db_error)?;
        let claimed = match claimed {
            ClaimResult::Idle => return Ok(false),
            ClaimResult::Progress => return Ok(true),
            ClaimResult::Ready(claimed) => *claimed,
        };
        if !self.capacity_allowed(&key).await? {
            self.fail_work(
                &key,
                claimed,
                ModelError::new(
                    "storage_limit",
                    "Logical data limit reached before embedding; retry after collection",
                ),
                false,
            )
            .await?;
            return Ok(true);
        }
        let connection_name = claimed
            .space
            .as_ref()
            .map(|s| s.profile.as_str())
            .or(self.config.embedding.connection.as_deref())
            .ok_or_else(|| err(ErrorCode::ConfigInvalid, "Embedding connection missing"))?;
        let model = claimed
            .space
            .as_ref()
            .map(|s| s.model.as_str())
            .or(self.config.embedding.model.as_deref())
            .ok_or_else(|| err(ErrorCode::ConfigInvalid, "Embedding model missing"))?;
        let result = if self.config.connections.contains_key(connection_name) {
            match self.clients.get(connection_name) {
                Ok(client) => {
                    client
                        .embeddings(
                            model,
                            std::slice::from_ref(&claimed.work.segment.text),
                            claimed.space.as_ref().map(|s| s.dimension),
                            cancel.clone(),
                        )
                        .await
                }
                Err(error) => Err(error),
            }
        } else {
            Err(ModelError::new(
                "config_missing",
                "Embedding connection no longer exists",
            ))
        };
        if cancel.is_cancelled() {
            self.fail_work(
                &key,
                claimed,
                ModelError::new("cancelled", "Embedding cancelled; retry explicitly"),
                true,
            )
            .await?;
            return Ok(true);
        }
        match result {
            Ok(mut response) => {
                if response.dimension > crate::storage::MAX_VECTOR_DIMENSIONS {
                    self.fail_work(&key, claimed, ModelError::new("dimension_mismatch", "sqlite-vec v0.1.9 supports at most8192 dimensions; choose a supported embedding profile"), false).await?;
                    return Ok(true);
                }
                let vector = response.vectors.remove(0);
                let embedding = identity::EmbeddingIdentity::configured(&self.config, key.clone());
                let failure_key = self.failure_key().unwrap_or(&key).to_owned();
                let recovery_claim = claimed.clone();
                let adoption = store::EmbeddingAdoption {
                    failure_key,
                    claimed,
                    embedding,
                    dimension: response.dimension,
                    vector,
                };
                let adopted = self
                    .database
                    .call(move |db| {
                        let tx = db.transaction()?;
                        adoption.adopt_embedding_tx(&tx)?;
                        tx.commit()?;
                        Ok(())
                    })
                    .await;
                if let Err(error) = adopted {
                    let error = error.into_api_error();
                    let kind = serde_json::to_value(error.code)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "invalid_state".into());
                    self.fail_work(
                        &key,
                        recovery_claim,
                        ModelError::new(&kind, error.message),
                        false,
                    )
                    .await?;
                }
            }
            Err(error) => self.fail_work(&key, claimed, error, false).await?,
        }
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
        assert!(service.process_one(CancellationToken::new()).await.is_err());
        assert!(service.storage_diagnostic_failed.load(Ordering::Acquire));
        let error = service.search("synthetic query", 10).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(
            error
                .message
                .contains("Index workers stopped after a storage failure")
        );
        assert!(!error.message.contains("owner stopped"));
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
        let status = service.status(IndexArgs::default()).await.unwrap();
        assert!(status.paused);
        assert!(
            status
                .reason
                .unwrap()
                .contains("diagnostic pause could not be saved")
        );
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
