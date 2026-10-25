//! Bounded raw retention and revision-checked history reachability collection.
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
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod history;
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
    lane: Arc<Mutex<()>>,
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
    pub summary_cache_parts_collected: u64,
    pub summary_cache_bytes_collected: u64,
    pub deferred: bool,
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
}
#[derive(Debug, Serialize, Deserialize)]
struct Progress {
    stage: Stage,
    last_rowid: i64,
    #[serde(default)]
    horizon: Option<i64>,
    #[serde(default)]
    revalidate_roots: bool,
}
#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Roots,
    Provenance,
    Sweep,
}
impl Stage {
    fn as_str(self) -> &'static str {
        match self {
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
}

impl GcService {
    pub fn new(
        database: DbWorker,
        config: Arc<Config>,
        data_dir: PathBuf,
        attachments: Arc<AttachmentService>,
    ) -> Self {
        Self {
            database,
            files: BlockingFiles::new(config.workers.control_file),
            config,
            data_dir,
            attachments,
            lane: Arc::new(Mutex::new(())),
        }
    }
    pub async fn wait_idle(&self) -> Result<()> {
        self.files.wait_idle().await
    }
    pub async fn status(&self) -> Result<StorageStatus> {
        Ok(self.details().await?.status)
    }
    pub async fn details(&self) -> Result<StorageDetails> {
        let logical = self
            .database
            .call(|db| Ok(logical_usage(db)))
            .await
            .map_err(crate::app::storage_error)??;
        let raw = self.raw_stats(Utc::now()).await?;
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
        self.database
            .call(move |db| Ok(require_capacity_tx(db, &config)))
            .await
            .map_err(crate::app::storage_error)?
    }
    pub async fn collect(
        &self,
        category: StorageCategory,
        at: DateTime<Utc>,
    ) -> Result<CollectionReport> {
        let _lane = self.lane.lock().await;
        let mut report = CollectionReport::default();
        if matches!(category, StorageCategory::All | StorageCategory::Raw) {
            self.collect_raw(at, &mut report).await?;
        }
        if matches!(category, StorageCategory::All | StorageCategory::History)
            && let Some(job) = self.start_history().await?
        {
            let mut restarts = 0;
            loop {
                let step = self.history_step(&job).await?;
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
                tokio::task::yield_now().await;
            }
        }
        if matches!(category, StorageCategory::All | StorageCategory::History) {
            loop {
                let (count,bytes)=self.database.call(|db| {
                    let tx=db.transaction()?;
                    // A successful part is reusable only for exact source revisions.
                    // Once any source was explicitly deletion-GC'd it cannot match.
                    let rows=tx.prepare("SELECT cache.key,length(CAST(cache.value AS BLOB)) FROM app_meta cache WHERE cache.key LIKE 'summary.part.%' AND EXISTS(SELECT 1 FROM json_each(CASE WHEN json_valid(cache.value) THEN cache.value ELSE '{}' END,'$.sources') s LEFT JOIN node n ON n.id=s.value WHERE n.id IS NULL OR (n.payload_json IS NULL AND n.deleted_reason IS NOT NULL)) ORDER BY key LIMIT 128")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)? as u64)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    let bytes=rows.iter().map(|r|r.1).sum::<u64>();
                    for (key,_) in &rows {tx.execute("DELETE FROM app_meta WHERE key=?1",[key])?;}
                    tx.commit()?;Ok((rows.len()as u64,bytes))
                }).await.map_err(crate::app::storage_error)?;
                report.summary_cache_parts_collected += count;
                report.summary_cache_bytes_collected += bytes;
                if count < BATCH as u64 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        if matches!(category, StorageCategory::All | StorageCategory::Index) {
            loop {
                let removed=self.database.call(|db| {
                    let tx=db.transaction()?;
                    let nodes=tx.prepare("SELECT n.id FROM node n WHERE (n.payload_json IS NULL OR n.deleted_reason IS NOT NULL) AND (EXISTS(SELECT 1 FROM embedding_job WHERE source_node_id=n.id) OR EXISTS(SELECT 1 FROM embedding_chunk WHERE source_node_id=n.id)) ORDER BY n.id LIMIT 128")?.query_map([],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    for node in &nodes {if let Err(error)=crate::index::delete_source_vectors_tx(&tx,node){return Ok(Err(error));}}
                    let count=nodes.len() as u64;tx.commit()?;Ok(Ok(count))
                }).await.map_err(crate::app::storage_error)??;
                report.index_sources_collected += removed;
                if removed < BATCH as u64 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        if matches!(
            category,
            StorageCategory::All | StorageCategory::ManagedImages
        ) {
            let managed = self.attachments.collect_managed(at).await?;
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
    async fn raw_stats(&self, at: DateTime<Utc>) -> Result<(u64, u64, u64, u64)> {
        let cutoff = cutoff(at, self.config.retention.tool_raw_days).map(|v| v.to_rfc3339());
        self.database.call(move|db| {
            let row=db.query_row("WITH sizes AS (SELECT s.*,COALESCE((SELECT SUM(length(bytes)) FROM output_chunk WHERE execution_id=s.id),0) AS size,EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id=s.id) AS pinned,EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||s.id) AS marked FROM shell_execution s), eligible AS(SELECT *,state IN ('exited','failed','stopped') AND NOT pinned AS eligible FROM sizes) SELECT COALESCE(SUM(size),0),COALESCE(SUM(CASE WHEN NOT eligible THEN size ELSE 0 END),0),COALESCE(SUM(CASE WHEN eligible THEN size ELSE 0 END),0),COALESCE(SUM(CASE WHEN eligible AND (marked OR julianday(COALESCE(ended_at,started_at))<=julianday(?1)) THEN size ELSE 0 END),0) FROM eligible",[cutoff],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64,r.get::<_,i64>(2)? as u64,r.get::<_,i64>(3)? as u64)))?;
            Ok(row)
        }).await.map_err(crate::app::storage_error)
    }
    async fn raw_batch(&self, after: Option<(String, i64)>) -> Result<Vec<RawExecution>> {
        let (key, rowid) = after.unwrap_or_default();
        self.database.call(move|db| {
            let mut statement=db.prepare("SELECT s.id,s.state,s.ended_at,s.started_at,s.rowid,COALESCE(s.ended_at,s.started_at),EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||s.id) FROM shell_execution s WHERE (COALESCE(s.ended_at,s.started_at)>?1 OR (COALESCE(s.ended_at,s.started_at)=?1 AND s.rowid>?2)) AND s.state IN ('exited','failed','stopped') AND NOT EXISTS(SELECT 1 FROM runtime_pin WHERE resource_kind='shell_output' AND resource_id=s.id) AND (EXISTS(SELECT 1 FROM output_chunk WHERE execution_id=s.id) OR EXISTS(SELECT 1 FROM app_meta WHERE key='raw_tombstone.'||s.id AND json_extract(value,'$.state')='deleting')) ORDER BY COALESCE(s.ended_at,s.started_at),s.rowid LIMIT 128")?;
            Ok(statement.query_map(params![key,rowid],|row|Ok(RawExecution{id:row.get(0)?,ended:row.get(2)?,started:row.get(3)?,rowid:row.get(4)?,order_key:row.get(5)?,marked:row.get(6)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await.map_err(crate::app::storage_error)
    }
    async fn collect_raw(&self, at: DateTime<Utc>, report: &mut CollectionReport) -> Result<()> {
        let stats = self.raw_stats(at).await?;
        let mut total = stats.0;
        report.protected_raw_bytes = stats.1;
        let mut after = None;
        let cutoff = cutoff(at, self.config.retention.tool_raw_days);
        loop {
            let candidates = self.raw_batch(after.clone()).await?;
            if candidates.is_empty() {
                break;
            }
            for candidate in candidates {
                after = Some((candidate.order_key.clone(), candidate.rowid));
                let expired = cutoff.is_some_and(|cutoff| {
                    DateTime::parse_from_rfc3339(
                        candidate.ended.as_deref().unwrap_or(&candidate.started),
                    )
                    .is_ok_and(|ended| ended.with_timezone(&Utc) <= cutoff)
                });
                if !candidate.marked && !expired && total <= self.config.retention.tool_raw_bytes {
                    continue;
                }
                let reason = if expired {
                    "raw_retention_age"
                } else {
                    "raw_retention_capacity"
                };
                let execution = candidate.id.clone();
                let at = at.to_rfc3339();
                let reason = reason.to_owned();
                let started = self
                    .database
                    .call(move |db| Ok(begin_raw(db, &execution, &reason, &at)))
                    .await
                    .map_err(crate::app::storage_error)??;
                if !started {
                    report.deferred = true;
                    continue;
                }
                loop {
                    let execution = candidate.id.clone();
                    let step = self
                        .database
                        .call(move |db| Ok(raw_step(db, &execution)))
                        .await
                        .map_err(crate::app::storage_error)??;
                    report.raw_bytes_collected += step.0;
                    total = total.saturating_sub(step.0);
                    if step.1 {
                        break;
                    }
                    if step.2 {
                        report.deferred = true;
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
        }
        if total > self.config.retention.tool_raw_bytes {
            report.deferred = true;
        }
        Ok(())
    }
    /// Start/restart marking for an explicit Thread-deletion GC job. The TEMP
    /// marks are disposable; durable progress is never trusted without them.
    pub async fn start_history(&self) -> Result<Option<String>> {
        self.database
            .call(|db| Ok(start_history(db)))
            .await
            .map_err(crate::app::storage_error)?
    }
    pub async fn history_step(&self, job_id: &str) -> Result<HistoryStep> {
        let job = job_id.to_owned();
        self.database
            .call(move |db| Ok(history_step(db, &job)))
            .await
            .map_err(crate::app::storage_error)?
    }
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
    tx.execute("UPDATE gc_job SET root_revision=?2,cursor_json=?3,state='running',error_json=NULL WHERE id=?1",params![job,root,serde_json::to_string(&Progress{stage:Stage::Roots,last_rowid:0,horizon:Some(horizon),revalidate_roots:false}).unwrap()]).map_err(sql_error)?;
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
            .is_some_and(|progress| progress.horizon.is_some());
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
