//! Path references, bounded preparation, selection CAS, and managed image lifecycle.
//! All filesystem/clipboard work runs on bounded blocking workers, never the UI.
pub mod clipboard;
mod draft;
mod registration;
mod selection;
use selection::snapshot_selection_tx;
mod success;
use crate::{
    config::Config,
    protocol::{
        ApiError, AttachmentDetails, AttachmentKind, DEFAULT_LIST_LIMIT, ErrorCode, MAX_BODY_BYTES,
    },
    storage::{DbWorker, StorageError},
};
use base64::{Engine, engine::general_purpose::STANDARD};
pub(crate) use draft::DraftContent;
pub use registration::add_tx;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::SystemTime,
};
use success::mark_success_rows_tx;
pub use success::mark_success_tx;
use tokio::{
    sync::Semaphore,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn io_error(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::IoFailed, message)
}
fn sql_error(error: rusqlite::Error) -> ApiError {
    crate::storage::save_error(error.to_string())
}
fn storage_error(error: StorageError) -> ApiError {
    error.into_api_error()
}
fn stale() -> ApiError {
    ApiError::new(
        ErrorCode::OperationConflict,
        "Attachment selection changed; prepare again before starting an API request",
    )
}
fn cancelled() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidState,
        "Attachment preparation cancelled; no API request may start",
    )
}

#[derive(Clone)]
pub struct BlockingFiles {
    slots: Arc<Semaphore>,
    capacity: usize,
}
impl BlockingFiles {
    pub fn new(workers: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(workers.max(1))),
            capacity: workers.max(1),
        }
    }
    pub fn available(&self) -> usize {
        self.slots.available_permits()
    }
    pub async fn wait_idle(&self) -> Result<(), ApiError> {
        let capacity = u32::try_from(self.capacity).map_err(|_| {
            ApiError::new(
                ErrorCode::ConfigInvalid,
                "File worker count exceeds supported semaphore capacity",
            )
        })?;
        let _all = self
            .slots
            .clone()
            .acquire_many_owned(capacity)
            .await
            .map_err(|_| io_error("File worker pool closed during shutdown"))?;
        Ok(())
    }
    /// A timed-out started closure continues holding its permit until actual exit.
    pub async fn execute<T, F>(
        &self,
        deadline: Instant,
        cancel: CancellationToken,
        operation: F,
    ) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    {
        let permit = tokio::select! { biased; _=cancel.cancelled()=>return Err(cancelled()), permit=tokio::time::timeout_at(deadline,self.slots.clone().acquire_owned())=>permit.map_err(|_|io_error("File preparation timed out waiting for a worker"))?.map_err(|_|io_error("File workers closed"))? };
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        });
        tokio::select! { biased; _=cancel.cancelled()=>Err(cancelled()), result=tokio::time::timeout_at(deadline,task)=>result.map_err(|_|io_error("File preparation timed out; the OS read may still occupy a worker"))?.map_err(|_|io_error("File worker failed"))? }
    }
}
#[derive(Clone, Debug)]
pub struct AttachmentInput {
    pub path: PathBuf,
    pub kind: AttachmentKind,
}
#[derive(Clone, Debug)]
pub struct PlannedAttachment {
    pub path: PathBuf,
    pub kind: AttachmentKind,
    pub observed_bytes: Option<u64>,
    pub managed_id: Option<String>,
}
#[derive(Clone, Debug)]
pub struct AddPlan {
    pub thread_id: String,
    pub selection_revision: i64,
    pub files: Vec<PlannedAttachment>,
}
#[derive(Clone, Debug)]
struct Selected {
    id: String,
    source_node_id: String,
    source_element_index: usize,
    path: PathBuf,
    kind: AttachmentKind,
    protected: bool,
    active: bool,
    reason: Option<String>,
    previous_hash: Option<String>,
    previously_included: bool,
    detached: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachmentManifest {
    pub attachment_id: String,
    pub source_node_id: String,
    pub source_element_index: usize,
    pub api_item_index: Option<usize>,
    pub api_content_index: Option<usize>,
    pub api_content_count: usize,
    pub first_inclusion: bool,
    pub previous_hash: Option<String>,
    pub path: PathBuf,
    pub kind: String,
    pub state: String,
    pub raw_size: Option<u64>,
    pub content_hash: Option<String>,
    pub mime_type: Option<String>,
    pub changed: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct PreparedPlacement {
    pub attachment_id: String,
    pub source_node_id: String,
    pub source_element_index: usize,
    pub content: Vec<Value>,
    pub(crate) detached: bool,
}
/// Keep this value alive throughout the HTTP attempt: it owns managed read/send pins.
pub struct PreparedAttachments {
    pub thread_id: String,
    pub selection_revision: i64,
    pub placements: Vec<PreparedPlacement>,
    pub notices: Vec<String>,
    pub excluded_ids: Vec<String>,
    pub manifest: Vec<AttachmentManifest>,
    _pins: Arc<PinLease>,
    _resources: Vec<Arc<Encoded>>,
}
impl std::fmt::Debug for PreparedAttachments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedAttachments")
            .field("thread_id", &self.thread_id)
            .field("selection_revision", &self.selection_revision)
            .field("manifest", &self.manifest)
            .field("excluded_ids", &self.excluded_ids)
            .finish_non_exhaustive()
    }
}
struct PinLease {
    database: DbWorker,
    ids: Vec<String>,
}
impl Drop for PinLease {
    fn drop(&mut self) {
        if self.ids.is_empty() {
            return;
        }
        let database = self.database.clone();
        let ids = self.ids.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = database
                    .call(move |db| {
                        let tx = db.transaction()?;
                        for id in ids {
                            tx.execute("DELETE FROM runtime_pin WHERE id=?1", [id])?;
                        }
                        tx.commit()?;
                        Ok(())
                    })
                    .await;
            });
        }
        // If runtime is gone, startup cleanup removes old-instance pins.
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    bytes: u64,
    modified: Option<SystemTime>,
}
#[derive(Debug)]
struct Encoded {
    base64: String,
    hash: String,
    raw_size: u64,
    mime: String,
}
impl Encoded {
    fn bytes(&self) -> usize {
        self.base64.len() + self.hash.len() + self.mime.len()
    }
}
struct CacheEntry {
    signature: Signature,
    content: Arc<Encoded>,
    touched: u64,
}
#[derive(Default)]
struct Cache {
    budget: usize,
    entries: BTreeMap<PathBuf, CacheEntry>,
    bytes: usize,
    clock: u64,
    objects: Vec<Weak<Encoded>>,
}
#[derive(Clone, Debug, Default)]
pub struct CacheStats {
    pub retained_bytes: usize,
    pub live_encoded_bytes: usize,
    pub pinned_bytes: usize,
    pub entries: usize,
}
#[derive(Clone)]
pub struct AttachmentService {
    database: DbWorker,
    config: Config,
    data_dir: PathBuf,
    instance_id: String,
    pub workers: BlockingFiles,
    control_workers: BlockingFiles,
    cache: Arc<Mutex<Cache>>,
}
impl AttachmentService {
    pub fn new(
        database: DbWorker,
        config: &Config,
        data_dir: PathBuf,
        instance_id: String,
    ) -> Self {
        Self {
            database,
            config: config.clone(),
            data_dir,
            instance_id,
            workers: BlockingFiles::new(config.workers.attachment),
            control_workers: BlockingFiles::new(config.workers.control_file),
            cache: Arc::new(Mutex::new(Cache {
                budget: config.cache.attachment_base64_bytes as usize,
                ..Cache::default()
            })),
        }
    }
    pub async fn wait_idle(&self) -> Result<(), ApiError> {
        tokio::try_join!(self.workers.wait_idle(), self.control_workers.wait_idle())?;
        Ok(())
    }
    pub fn cache_stats(&self) -> CacheStats {
        let mut cache = self.cache.lock().expect("attachment cache poisoned");
        cache.objects.retain(|o| o.strong_count() > 0);
        let retained: HashSet<_> = cache
            .entries
            .values()
            .map(|e| Arc::as_ptr(&e.content))
            .collect();
        let mut stats = CacheStats {
            retained_bytes: cache.bytes,
            entries: cache.entries.len(),
            ..CacheStats::default()
        };
        for object in &cache.objects {
            if let Some(object) = object.upgrade() {
                stats.live_encoded_bytes += object.bytes();
                if Arc::strong_count(&object)
                    > if retained.contains(&Arc::as_ptr(&object)) {
                        2
                    } else {
                        1
                    }
                {
                    stats.pinned_bytes += object.bytes();
                }
            }
        }
        stats
    }
    fn deadline(&self) -> Instant {
        Instant::now() + Duration::from_millis(self.config.io.file_prepare_timeout_ms)
    }
    pub async fn plan_add(
        &self,
        thread_id: &str,
        files: Vec<AttachmentInput>,
        cancel: CancellationToken,
    ) -> Result<AddPlan, ApiError> {
        let thread = thread_id.to_owned();
        let (cwd,revision,protected)=self.database.call(move|db|{
            let row:Option<(String,i64)>=db.query_row("SELECT cwd,selection_revision FROM thread WHERE id=?1 AND deleted_at IS NULL",[&thread],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((cwd,revision))=row else{return Ok(None);};
            let mut statement=db.prepare("SELECT a.absolute_path FROM attachment_selection s JOIN attachment_ref a ON a.id=s.attachment_id WHERE s.thread_id=?1 AND s.state='active' AND s.protected=1")?;
            let paths=statement.query_map([&thread],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;Ok(Some((PathBuf::from(cwd),revision,paths)))
        }).await.map_err(storage_error)?.ok_or_else(||ApiError::new(ErrorCode::ThreadNotFound,"Attachment target Thread was not found or was deleted"))?;
        let limit = self.config.limits.attachment_request_bytes;
        let file_limit = self.config.limits.attachment_file_bytes;
        let deadline = self.deadline();
        let planned=self.workers.execute(deadline,cancel,move||{
            let mut total=0u64;
            for path in protected { if let Ok(meta)=std::fs::metadata(path) && meta.is_file() && meta.len() <= file_limit { total=total.checked_add(meta.len()).ok_or_else(||io_error("Attachment byte count overflow"))?; } }
            let mut result=Vec::new();
            for file in files { let path=if file.path.is_absolute(){file.path}else{cwd.join(file.path)}; if path.as_os_str().is_empty(){return Err(io_error("Attachment path is empty"));}
                let bytes=std::fs::metadata(&path).ok().filter(|m|m.is_file()).map(|m|m.len());
                total=total.checked_add(bytes.filter(|bytes| *bytes <= file_limit).unwrap_or(0)).ok_or_else(||io_error("Attachment byte count overflow"))?;
                result.push(PlannedAttachment{path,kind:file.kind,observed_bytes:bytes,managed_id:None});
            }
            if total>limit{return Err(ApiError::new(ErrorCode::InputLimit,"New protected attachments exceed the request raw-byte limit; reduce or split the attachments").details(json!({"protected_bytes":total,"limit_bytes":limit})));}Ok(result)
        }).await?;
        Ok(AddPlan {
            thread_id: thread_id.into(),
            selection_revision: revision,
            files: planned,
        })
    }
    /// Register references already declared in the immutable source payload.
    /// This does not append content to an existing node.
    pub async fn add(
        &self,
        plan: AddPlan,
        source_node_id: Option<String>,
    ) -> Result<Vec<AttachmentDetails>, ApiError> {
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                match add_tx(&tx, &plan, source_node_id.as_deref()) {
                    Ok(result) => {
                        tx.commit()?;
                        Ok(Ok(result))
                    }
                    Err(error) => Ok(Err(error)),
                }
            })
            .await
            .map_err(storage_error)?
    }
    pub async fn prepare(
        &self,
        thread_id: &str,
        cancel: CancellationToken,
        force_reload: bool,
    ) -> Result<PreparedAttachments, ApiError> {
        self.prepare_selected(thread_id, cancel, force_reload, None)
            .await
    }
    pub(crate) async fn prepare_at_revision(
        &self,
        thread_id: &str,
        cancel: CancellationToken,
        force_reload: bool,
        expected_revision: i64,
    ) -> Result<PreparedAttachments, ApiError> {
        self.prepare_selected(thread_id, cancel, force_reload, Some(expected_revision))
            .await
    }
    async fn prepare_selected(
        &self,
        thread_id: &str,
        cancel: CancellationToken,
        force_reload: bool,
        expected_revision: Option<i64>,
    ) -> Result<PreparedAttachments, ApiError> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let thread = thread_id.to_owned();
        let instance = self.instance_id.clone();
        let snapshot = self
            .database
            .call(move |db| {
                let tx = db.transaction()?;
                let snapshot = snapshot_selection_tx(&tx, &thread, &instance, expected_revision)?;
                tx.commit()?;
                Ok(snapshot)
            })
            .await
            .map_err(storage_error)?;
        let selection::SelectionSnapshot {
            revision,
            selected,
            pin_ids,
        } = snapshot;
        let pins = Arc::new(PinLease {
            database: self.database.clone(),
            ids: pin_ids,
        });
        let deadline = self.deadline();
        let mut read: BTreeMap<PathBuf, Result<Arc<Encoded>, String>> = BTreeMap::new();
        for selection in selected.iter().filter(|s| s.active) {
            if read.contains_key(&selection.path) {
                continue;
            }
            let path = selection.path.clone();
            let cache = self.cache.clone();
            let limit = self.config.limits.attachment_file_bytes;
            let pin = pins.clone();
            let result = self
                .workers
                .execute(deadline, cancel.clone(), move || {
                    let _pin = pin;
                    read_encoded(&path, limit, force_reload, &cache)
                })
                .await;
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            read.insert(selection.path.clone(), result.map_err(|e| e.message));
        }
        let mut protected = 0u64;
        let mut total = 0u64;
        for s in selected.iter().filter(|s| s.active) {
            if let Some(Ok(content)) = read.get(&s.path) {
                total = total
                    .checked_add(content.raw_size)
                    .ok_or_else(|| io_error("Attachment size overflow"))?;
                if s.protected {
                    protected = protected
                        .checked_add(content.raw_size)
                        .ok_or_else(|| io_error("Attachment size overflow"))?;
                }
            }
        }
        if protected > self.config.limits.attachment_request_bytes {
            return Err(ApiError::new(ErrorCode::InputLimit,"Current protected attachment files exceed the request limit; request is held for explicit retry").details(json!({"protected_bytes":protected,"limit_bytes":self.config.limits.attachment_request_bytes})));
        }
        let mut excluded = Vec::new();
        for s in selected.iter().filter(|s| s.active && !s.protected) {
            if total <= self.config.limits.attachment_request_bytes {
                break;
            }
            if let Some(Ok(content)) = read.get(&s.path) {
                total -= content.raw_size;
                excluded.push(s.id.clone());
            }
        }
        let mut result = PreparedAttachments {
            thread_id: thread_id.into(),
            selection_revision: revision,
            placements: Vec::new(),
            notices: Vec::new(),
            excluded_ids: excluded.clone(),
            manifest: Vec::new(),
            _pins: pins,
            _resources: Vec::new(),
        };
        let mut notice_bytes = serde_json::to_vec(&json!({"role":"developer","content":[]}))
            .unwrap()
            .len();
        let mut notice_content_count = 0;
        for s in selected {
            let notice_start = result.notices.len();
            let kind = if s.kind == AttachmentKind::Image {
                "image"
            } else {
                "file"
            };
            let mut expansion = Vec::new();
            let mut manifest = AttachmentManifest {
                attachment_id: s.id.clone(),
                source_node_id: s.source_node_id.clone(),
                source_element_index: s.source_element_index,
                api_item_index: None,
                api_content_index: None,
                api_content_count: 0,
                first_inclusion: !s.previously_included && s.protected,
                previous_hash: s.previous_hash.clone(),
                path: s.path.clone(),
                kind: kind.into(),
                state: "ready".into(),
                raw_size: None,
                content_hash: None,
                mime_type: None,
                changed: false,
                error: None,
            };
            if !s.active || excluded.contains(&s.id) {
                manifest.state = "excluded".into();
                let reason = if excluded.contains(&s.id) {
                    "request raw-byte limit"
                } else {
                    s.reason.as_deref().unwrap_or("previously excluded")
                };
                let notice = if s.detached {
                    format!(
                        "Shellboy attachment exclusion v1: {}",
                        exclusion_notice(&s, thread_id, reason)
                    )
                } else {
                    format!(
                        "Attachment excluded from sending: {} ({reason}); reattach its path to include it again",
                        s.path.display()
                    )
                };
                result.notices.push(notice.clone());
                expansion.push(json!({"type":"input_text","text":notice}));
            } else {
                match &read[&s.path] {
                    Ok(content) => {
                        manifest.raw_size = Some(content.raw_size);
                        manifest.content_hash = Some(content.hash.clone());
                        manifest.mime_type = Some(content.mime.clone());
                        manifest.changed = s
                            .previous_hash
                            .as_ref()
                            .is_some_and(|hash| hash != &content.hash);
                        if manifest.changed {
                            let notice = format!(
                                "[Shellboy attachment changed update] id={} source={} previous_hash={} current_hash={}",
                                s.id,
                                s.source_node_id,
                                s.previous_hash.as_deref().unwrap_or("unknown"),
                                content.hash
                            );
                            result.notices.push(notice.clone());
                        }
                        let data = format!("data:{};base64,{}", content.mime, content.base64);
                        expansion.push(if s.kind==AttachmentKind::Image{json!({"type":"input_image","image_url":data,"detail":"auto"})}else{json!({"type":"input_file","filename":s.path.file_name().unwrap_or_default().to_string_lossy(),"file_data":data})});
                        result._resources.push(content.clone());
                    }
                    Err(error) => {
                        manifest.state = "unavailable".into();
                        manifest.error = Some(error.clone());
                        let notice = format!(
                            "Attachment unavailable: {} ({error}); continue with the remaining conversation",
                            s.path.display()
                        );
                        result.notices.push(notice.clone());
                        expansion.push(json!({"type":"input_text","text":notice}));
                    }
                }
            }
            let metadata = json!({"type":"input_text","text":format!("[Shellboy attachment metadata] {}",json!({"attachment_id":s.id,"path":s.path,"unavailable_reason":manifest.error.as_deref().or(s.reason.as_deref()),"source_node_id":s.source_node_id,"source_element_index":s.source_element_index,"content_hash":manifest.content_hash,"previous_hash":manifest.previous_hash,"state":manifest.state,"first_inclusion":manifest.first_inclusion,"version":"current read for this request"}))});
            let mut content = vec![metadata];
            content.extend(expansion);
            if s.detached {
                // Account for the complete extra developer item, including JSON escaping.
                let bytes = serde_json::to_vec(&content).unwrap().len() - 2
                    + usize::from(notice_content_count > 0);
                if notice_bytes + bytes > MAX_BODY_BYTES as usize {
                    result.notices.truncate(notice_start);
                    continue;
                }
                notice_bytes += bytes;
                notice_content_count += content.len();
            }
            manifest.api_content_count = content.len();
            result.placements.push(PreparedPlacement {
                attachment_id: s.id,
                source_node_id: s.source_node_id,
                source_element_index: s.source_element_index,
                content,
                detached: s.detached,
            });
            result.manifest.push(manifest);
        }
        Ok(result)
    }
    /// Call only after successful HTTP acceptance/completion and the driver's
    /// generation check. Failed attempts must not change comparison baselines.
    pub async fn mark_success(
        &self,
        prepared: &PreparedAttachments,
        attempt_id: &str,
    ) -> Result<(), ApiError> {
        let attempt = attempt_id.to_owned();
        let thread = prepared.thread_id.clone();
        let ids: Vec<_> = prepared
            .manifest
            .iter()
            .map(|m| m.attachment_id.clone())
            .collect();
        self.database
            .call(move |db| {
                let tx = db.transaction()?;
                mark_success_rows_tx(&tx, &thread, &attempt, &ids).map_err(StorageError::from)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(storage_error)
    }
}

/// Exclude binaries whose original nodes leave the adopted conversation path.
/// The compaction caller validates the old selection and adopts its new path in
/// this transaction; select_path performs the single thread revision increment.
pub fn exclude_replaced_sources_tx(
    tx: &Transaction<'_>,
    thread: &str,
    sources: &[String],
) -> Result<usize, StorageError> {
    let mut removed = 0;
    let mut statement=tx.prepare("UPDATE attachment_selection SET state='excluded',reason='context_replacement',protected=0,revision=revision+1 WHERE thread_id=?1 AND state='active' AND attachment_id IN (SELECT id FROM attachment_ref WHERE source_node_id=?2)")?;
    for source in sources {
        removed += statement.execute(params![thread, source])?;
    }
    Ok(removed)
}

/// Bounded path metadata, never a statement that file bytes were read.
fn exclusion_notice(selection: &Selected, thread: &str, reason: &str) -> Value {
    let path = selection.path.to_string_lossy();
    let end = path.floor_char_boundary(path.len().min(MAX_BODY_BYTES as usize / 16));
    let path_hash = crate::digest::sha256_hex(path.as_bytes());
    json!({
        "attachment_id": selection.id, "source_node_id": selection.source_node_id,
        "path": &path[..end], "path_truncated": end < path.len(),
        "path_utf8_bytes": path.len(), "path_utf8_sha256": path_hash,
        "reason": reason, "binary_included": false,
        "recovery": "reattach the path with a new attachment ID",
        "details_command": format!("shellboy attachment list --thread {thread}")
    })
}

fn delivery_status(db: &Connection, thread: &str, source: &str) -> rusqlite::Result<String> {
    db.query_row("SELECT CASE WHEN EXISTS(SELECT 1 FROM thread_path WHERE thread_id=?1 AND node_id=?2) THEN 'applied' ELSE COALESCE((SELECT status FROM attachment_delivery WHERE thread_id=?1 AND node_id=?2),'accepted') END",params![thread,source],|r|r.get(0))
}
pub const ATTACHMENT_LIST_SQL: &str = "SELECT a.id,a.absolute_path,a.input_kind,a.ownership,s.state,s.reason,a.source_node_id,m.state,m.deleted_reason FROM attachment_selection s JOIN attachment_ref a ON a.id=s.attachment_id LEFT JOIN managed_file m ON m.absolute_path=a.absolute_path WHERE s.thread_id=?1 ORDER BY s.selected_order,a.id";

/// Filesystem observations run only on a file/display worker, not the DB lane.
pub(crate) fn observe_paths(items: &mut [AttachmentDetails]) {
    for item in items {
        if item.unavailable_reason.is_some() {
            continue;
        }
        item.unavailable_reason = match std::fs::metadata(&item.path) {
            Ok(meta) if meta.is_file() => {
                std::fs::File::open(&item.path).err().map(|e| e.to_string())
            }
            Ok(_) => Some("Attachment path is not a regular file".into()),
            Err(error) => Some(error.to_string()),
        };
    }
}

pub fn list_tx(db: &Connection, thread_id: &str) -> Result<Vec<AttachmentDetails>, ApiError> {
    let mut statement = db.prepare(ATTACHMENT_LIST_SQL).map_err(sql_error)?;
    statement
        .query_map([thread_id], |r| {
            let kind: String = r.get(2)?;
            let source: String = r.get(6)?;
            Ok(AttachmentDetails {
                delivery_status: delivery_status(db, thread_id, &source)?,
                source_node_id: source,
                attachment_id: r.get(0)?,
                thread_id: thread_id.into(),
                path: r.get(1)?,
                kind: if kind == "image" {
                    AttachmentKind::Image
                } else {
                    AttachmentKind::File
                },
                managed: r.get::<_, String>(3)? == "managed_clipboard",
                selected: r.get::<_, String>(4)? == "active",
                unavailable_reason: match r.get::<_, Option<String>>(7)? {
                    Some(state) if state != "ready" => Some(format!(
                        "{} ({state})",
                        r.get::<_, Option<String>>(8)?
                            .unwrap_or_else(|| "Managed attachment unavailable".into())
                    )),
                    _ => r.get(5)?,
                },
            })
        })
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)
}
pub fn remove_tx(
    tx: &Transaction<'_>,
    thread_id: &str,
    attachment_id: &str,
) -> Result<(), ApiError> {
    let changed=tx.execute("UPDATE attachment_selection SET state='excluded',reason='explicitly removed',revision=revision+1 WHERE thread_id=?1 AND attachment_id=?2 AND state='active'",params![thread_id,attachment_id]).map_err(sql_error)?;
    if changed == 0 {
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM attachment_selection WHERE thread_id=?1 AND attachment_id=?2)",params![thread_id,attachment_id],|r|r.get(0)).map_err(sql_error)?;
        if !exists {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Attachment does not belong to this Thread",
            ));
        }
    }
    if changed > 0 {
        tx.execute("UPDATE thread SET selection_revision=selection_revision+1,revision=revision+1 WHERE id=?1",[thread_id]).map_err(sql_error)?;
    }
    Ok(())
}
/// Same transaction as attempt admission, before any API side effect.
pub fn adopt_prepared_tx(
    tx: &Transaction<'_>,
    prepared: &PreparedAttachments,
    attempt_id: &str,
) -> Result<i64, ApiError> {
    let revision: i64 = crate::history::selection_revision(
        tx,
        &prepared.thread_id,
        crate::history::ThreadScope::Live,
    )
    .map_err(sql_error)?;
    if revision != prepared.selection_revision {
        return Err(stale());
    }
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM model_attempt a JOIN turn u ON u.id=a.turn_id JOIN run r ON r.id=u.run_id WHERE a.id=?1 AND u.thread_id=?2 AND a.generation=u.generation AND a.state IN ('preparing','inflight') AND u.state IN ('preparing','running','waiting') AND r.state='live')", params![attempt_id,prepared.thread_id], |row|row.get(0)).map_err(sql_error)?;
    if !valid {
        return Err(stale());
    }
    for id in &prepared.excluded_ids {
        tx.execute("UPDATE attachment_selection SET state='excluded',reason='request raw-byte limit',revision=revision+1 WHERE thread_id=?1 AND attachment_id=?2 AND state='active' AND protected=0",params![prepared.thread_id,id]).map_err(sql_error)?;
    }
    if !prepared.excluded_ids.is_empty() {
        tx.execute("UPDATE thread SET selection_revision=selection_revision+1,revision=revision+1 WHERE id=?1",[&prepared.thread_id]).map_err(sql_error)?;
    }
    for (ordinal, m) in prepared.manifest.iter().enumerate() {
        if m.api_item_index.is_none() || m.api_content_index.is_none() || m.api_content_count == 0 {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Attachment expansion must be bound to its API source before attempt admission",
            ));
        }
        tx.execute("INSERT INTO attachment_send(attempt_id,ordinal,attachment_id,state,raw_size,content_hash,mime_type,error_json)VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![attempt_id,ordinal as i64,m.attachment_id,m.state,m.raw_size.map(|v|i64::try_from(v).unwrap_or(i64::MAX)),m.content_hash,m.mime_type,m.error.as_ref().map(|e|json!({"message":e}).to_string())]).map_err(sql_error)?;
    }
    let adopted = revision
        + if prepared.excluded_ids.is_empty() {
            0
        } else {
            1
        };
    tx.execute(
        "UPDATE model_attempt SET selection_revision=?2 WHERE id=?1",
        params![attempt_id, adopted],
    )
    .map_err(sql_error)?;
    Ok(adopted)
}
fn read_encoded(
    path: &Path,
    limit: u64,
    force: bool,
    cache: &Mutex<Cache>,
) -> Result<Arc<Encoded>, ApiError> {
    let meta = std::fs::metadata(path).map_err(|e| io_error(e.to_string()))?;
    if !meta.is_file() {
        return Err(io_error("Attachment path is not a regular file"));
    }
    if meta.len() > limit {
        return Err(ApiError::new(
            ErrorCode::InputLimit,
            "Attachment exceeds the single-file raw-byte limit",
        ));
    }
    let signature = Signature {
        bytes: meta.len(),
        modified: meta.modified().ok(),
    };
    {
        let mut cache = cache.lock().expect("attachment cache poisoned");
        cache.clock += 1;
        let clock = cache.clock;
        if !force
            && let Some(entry) = cache.entries.get_mut(path)
            && entry.signature == signature
        {
            entry.touched = clock;
            return Ok(entry.content.clone());
        }
    }
    let mut file = File::open(path).map_err(|e| io_error(e.to_string()))?;
    let mut bytes = Vec::new();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| io_error(e.to_string()))?;
        if count == 0 {
            break;
        }
        if (bytes.len() as u64).saturating_add(count as u64) > limit {
            return Err(ApiError::new(
                ErrorCode::InputLimit,
                "Attachment grew beyond its single-file limit during read",
            ));
        }
        hash.update(&buffer[..count]);
        bytes.extend_from_slice(&buffer[..count]);
    }
    let hash = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let mime = mime_type(path, &bytes).into();
    let encoded = Arc::new(Encoded {
        base64: STANDARD.encode(&bytes),
        hash,
        raw_size: bytes.len() as u64,
        mime,
    });
    let mut cache = cache.lock().expect("attachment cache poisoned");
    cache.clock += 1;
    let clock = cache.clock;
    cache.objects.retain(|object| object.strong_count() > 0);
    cache.objects.push(Arc::downgrade(&encoded));
    if let Some(old) = cache.entries.remove(path) {
        cache.bytes -= old.content.bytes();
    }
    let budget = cache.budget;
    if encoded.bytes() <= budget {
        while cache.bytes.saturating_add(encoded.bytes()) > budget {
            let oldest = cache
                .entries
                .iter()
                .min_by_key(|(_, e)| e.touched)
                .map(|(p, _)| p.clone());
            if let Some(oldest) = oldest {
                let old = cache.entries.remove(&oldest).expect("present cache entry");
                cache.bytes -= old.content.bytes();
            } else {
                break;
            }
        }
        cache.bytes += encoded.bytes();
        cache.entries.insert(
            path.into(),
            CacheEntry {
                signature,
                content: encoded.clone(),
                touched: clock,
            },
        );
    }
    Ok(encoded)
}
fn mime_type(path: &Path, bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "txt" | "md" | "rs" | "py" | "json" | "toml" | "csv" => "text/plain",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "application/octet-stream",
        }
    }
}
/// UI may use this extension hint without touching the filesystem; preparation
/// still observes current bytes and provider rejection remains explicit.
pub fn infer_kind(path: &Path) -> AttachmentKind {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => AttachmentKind::Image,
        _ => AttachmentKind::File,
    }
}

#[derive(Clone)]
pub struct ManagedImage {
    pub id: String,
    pub path: PathBuf,
    pub raw_size: u64,
    _pin: Arc<PinLease>,
}
impl std::fmt::Debug for ManagedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedImage")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("raw_size", &self.raw_size)
            .finish()
    }
}
impl ManagedImage {
    pub fn planned(&self) -> PlannedAttachment {
        PlannedAttachment {
            path: self.path.clone(),
            kind: AttachmentKind::Image,
            observed_bytes: Some(self.raw_size),
            managed_id: Some(self.id.clone()),
        }
    }
}
#[derive(Debug, Default)]
pub struct ManagedGcReport {
    pub deleted: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub protected_bytes: u64,
    pub remaining_bytes: u64,
}
impl AttachmentService {
    /// Keep ManagedImage alive until the draft is attached transactionally or
    /// discarded: its lease protects unselected images during draft preparation.
    pub async fn save_clipboard_pixels(
        &self,
        pixels: clipboard::ClipboardPixels,
        cancel: CancellationToken,
    ) -> Result<ManagedImage, ApiError> {
        pixels.validate()?;
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let managed = id();
        let path = self
            .data_dir
            .join("managed")
            .join("clipboard")
            .join(format!("{managed}.png"));
        let temporary = self.data_dir.join("tmp").join(format!("{managed}.png.tmp"));
        let row = managed.clone();
        let location = path.to_string_lossy().into_owned();
        let instance = self.instance_id.clone();
        let pin = id();
        let pin_id = pin.clone();
        self.database.call(move|db|{let tx=db.transaction()?;tx.execute("INSERT INTO managed_file(id,absolute_path,state,created_at)VALUES(?1,?2,'creating',?3)",params![row,location,now()])?;tx.execute("INSERT INTO runtime_pin(id,instance_id,resource_kind,resource_id,generation,created_at)VALUES(?1,?2,'managed_file',?3,0,?4)",params![pin_id,instance,row,now()])?;tx.commit()?;Ok(())}).await.map_err(storage_error)?;
        let lease = Arc::new(PinLease {
            database: self.database.clone(),
            ids: vec![pin],
        });
        let worker_lease = lease.clone();
        let output = path.clone();
        let limit = self.config.limits.attachment_file_bytes;
        let written = self
            .workers
            .execute(self.deadline(), cancel.clone(), move || {
                let _lease = worker_lease;
                let mut bytes = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut bytes, pixels.width, pixels.height);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    let mut writer = encoder
                        .write_header()
                        .map_err(|_| io_error("Cannot initialize PNG encoding"))?;
                    writer
                        .write_image_data(&pixels.rgba)
                        .map_err(|_| io_error("Cannot encode clipboard PNG"))?;
                }
                if bytes.len() as u64 > limit {
                    return Err(ApiError::new(
                        ErrorCode::InputLimit,
                        "Encoded clipboard PNG exceeds the single-attachment limit",
                    ));
                }
                std::fs::create_dir_all(output.parent().expect("managed parent"))
                    .map_err(|e| io_error(e.to_string()))?;
                std::fs::create_dir_all(temporary.parent().expect("temporary parent"))
                    .map_err(|e| io_error(e.to_string()))?;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options
                    .open(&temporary)
                    .map_err(|e| io_error(e.to_string()))?;
                use std::io::Write;
                file.write_all(&bytes)
                    .and_then(|_| file.sync_all())
                    .map_err(|e| io_error(e.to_string()))?;
                drop(file);
                std::fs::rename(&temporary, &output).map_err(|e| io_error(e.to_string()))?;
                // The file contents are durable before the rename. Persist both
                // directory entries before publishing this managed image as ready.
                #[cfg(unix)]
                for parent in [output.parent(), temporary.parent()] {
                    std::fs::File::open(parent.expect("managed image parent"))
                        .and_then(|directory| directory.sync_all())
                        .map_err(|e| io_error(e.to_string()))?;
                }
                Ok(bytes.len() as u64)
            })
            .await;
        let size = match written {
            Ok(size) if !cancel.is_cancelled() => size,
            other => {
                let reason = other.err().unwrap_or_else(cancelled);
                let row = managed.clone();
                let description = reason.message.clone();
                let _=self.database.call(move|db|{db.execute("UPDATE managed_file SET state='failed',deleted_reason=?2 WHERE id=?1 AND state='creating'",params![row,description])?;Ok(())}).await;
                return Err(reason);
            }
        };
        let row = managed.clone();
        let changed=self.database.call(move|db|Ok(db.execute("UPDATE managed_file SET state='ready',raw_size=?2 WHERE id=?1 AND state='creating'",params![row,size as i64])?)).await.map_err(storage_error)?;
        if changed != 1 {
            return Err(stale());
        }
        Ok(ManagedImage {
            id: managed,
            path,
            raw_size: size,
            _pin: lease,
        })
    }
    pub async fn capture_clipboard(
        &self,
        adapter: &clipboard::ClipboardAdapter,
        cancel: CancellationToken,
    ) -> Result<ManagedImage, ApiError> {
        let pixels = adapter
            .capture(self.config.io.file_prepare_timeout_ms, cancel.clone())
            .await?;
        self.save_clipboard_pixels(pixels, cancel).await
    }
    /// Startup only, before admitting new operations. Do not call while another
    /// owner can be running against this database.
    pub async fn restart_cleanup(&self) -> Result<ManagedGcReport, ApiError> {
        let instance = self.instance_id.clone();
        self.database.call(move|db|{let tx=db.transaction()?;tx.execute("DELETE FROM runtime_pin WHERE instance_id<>?1",[instance])?;tx.execute("UPDATE managed_file SET state='failed',deleted_reason='creation interrupted by restart' WHERE state='creating' AND NOT EXISTS(SELECT 1 FROM runtime_pin p WHERE p.resource_kind='managed_file' AND p.resource_id=managed_file.id)",[])?;tx.commit()?;Ok(())}).await.map_err(storage_error)?;
        self.collect_managed(chrono::Utc::now()).await
    }
    pub async fn collect_managed(
        &self,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ManagedGcReport, ApiError> {
        let cutoff = i64::try_from(self.config.retention.clipboard_days)
            .ok()
            .and_then(chrono::Duration::try_days)
            .and_then(|duration| at.checked_sub_signed(duration))
            .map(|date| date.to_rfc3339());
        let budget = self.config.retention.clipboard_bytes;
        let (candidates,protected,total)=self.database.call(move|db|{
            let tx=db.transaction()?;let mut statement=tx.prepare("SELECT m.id,m.absolute_path,m.state,COALESCE(m.raw_size,0),m.created_at,EXISTS(SELECT 1 FROM runtime_pin p WHERE p.resource_kind='managed_file' AND p.resource_id=m.id) OR EXISTS(SELECT 1 FROM attachment_ref a JOIN attachment_selection s ON s.attachment_id=a.id JOIN thread t ON t.id=s.thread_id WHERE a.absolute_path=m.absolute_path AND s.state='active' AND t.deleted_at IS NULL) OR EXISTS(SELECT 1 FROM draft d,json_each(d.body_json,'$.attachments') a WHERE d.state IN ('editing','accepting','submitting','submitted') AND (d.thread_id IS NULL OR EXISTS(SELECT 1 FROM thread t WHERE t.id=d.thread_id AND t.deleted_at IS NULL)) AND (json_extract(a.value,'$.id')=m.id OR json_extract(a.value,'$.path')=m.absolute_path)) OR EXISTS(SELECT 1 FROM draft d JOIN thread t ON t.id=d.thread_id JOIN attachment_ref a ON a.source_node_id=json_extract(d.body_json,'$.source_message_id') WHERE d.state='archived' AND json_extract(d.body_json,'$.kind')='cancelled_message' AND t.deleted_at IS NULL AND a.absolute_path=m.absolute_path) FROM managed_file m WHERE m.state IN ('ready','failed','deleting') ORDER BY m.created_at,m.id")?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,bool>(5)?)))?.collect::<Result<Vec<_>,_>>()?;drop(statement);
            let mut total=rows.iter().filter(|r|r.2=="ready").map(|r|r.3 as u64).sum::<u64>();let protected=rows.iter().filter(|r|r.5).map(|r|r.3 as u64).sum();let mut candidates=Vec::new();
            for (id,path,state,size,created,pinned) in rows {
                if pinned { continue; }
                if state=="deleting"||state=="failed"||cutoff.as_ref().is_some_and(|cutoff|created<=*cutoff)||total>budget{if state=="ready"{total=total.saturating_sub(size as u64);}tx.execute("UPDATE managed_file SET state='deleting',deleted_reason=COALESCE(deleted_reason,'clipboard retention') WHERE id=?1 AND state IN ('ready','failed','deleting')",[&id])?;candidates.push((id,PathBuf::from(path)));}
            }
            tx.commit()?;Ok((candidates,protected,total))
        }).await.map_err(storage_error)?;
        let mut report = ManagedGcReport {
            protected_bytes: protected,
            remaining_bytes: total,
            ..ManagedGcReport::default()
        };
        for (id, path) in candidates {
            let owned = self.data_dir.join("managed").join("clipboard");
            let expected = format!("{id}.png");
            if path.parent() != Some(owned.as_path())
                || path.file_name().and_then(|s| s.to_str()) != Some(expected.as_str())
            {
                report.failed.push((
                    id,
                    "Refused to unlink a path outside owned clipboard files".into(),
                ));
                continue;
            }
            let temporary = self.data_dir.join("tmp").join(format!("{id}.png.tmp"));
            let result = self
                .control_workers
                .execute(self.deadline(), CancellationToken::new(), move || {
                    for path in [path, temporary] {
                        match std::fs::remove_file(path) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(io_error(e.to_string())),
                        }
                    }
                    Ok(())
                })
                .await;
            match result {
                Ok(()) => {
                    let row = id.clone();
                    self.database.call(move|db|{db.execute("UPDATE managed_file SET state='deleted' WHERE id=?1 AND state='deleting'",[row])?;Ok(())}).await.map_err(storage_error)?;
                    report.deleted.push(id);
                }
                Err(error) => report.failed.push((id, error.message)),
            }
        }
        report.remaining_bytes = self
            .database
            .call(|db| {
                db.query_row(
                    "SELECT COALESCE(SUM(raw_size),0) FROM managed_file WHERE state<>'deleted'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|value| value as u64)
                .map_err(StorageError::from)
            })
            .await
            .map_err(storage_error)?;
        Ok(report)
    }
}
