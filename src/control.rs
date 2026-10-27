//! Independent operation admission and per-Thread drivers on one async runtime.
mod foreground;
pub(crate) mod interruption;
mod live;
mod runtime;
pub use foreground::ForegroundRelease;
use foreground::ForegroundSlot;
use live::LiveRegistry;
use runtime::{RuntimeOperation, encode_result};
pub mod attachment_delivery;
mod attachment_input;
pub(crate) mod context;
pub mod db;
mod driver;
pub mod output;
pub(crate) mod receipt;
pub mod replay;
pub(crate) mod state;
mod wait;

use crate::{
    app::{DataOwner, storage_error},
    instructions::InstructionLoader,
    ipc::Invocation,
    protocol::*,
    shell::{ShellHandle, ShellSupervisor},
};
use db::{MutationResult, Store};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Changed {
        thread_id: String,
        /// Committed transition fact for frontend attention; never a saved state.
        recovery: bool,
    },
    Stream {
        thread_id: String,
        node_id: String,
        text: String,
        reasoning: bool,
        stream_char_offset: usize,
    },
    /// Ephemeral transport observation; never persisted or included in model input.
    ModelReceive {
        thread_id: String,
        attempt_id: String,
        observed_at: std::time::Instant,
    },
    Notice {
        thread_id: String,
        foreground_release: Option<(String, ForegroundRelease)>,
        cancelled_calls: usize,
    },
    Error {
        thread_id: String,
        error: ApiError,
    },
}
#[derive(Clone)]
struct DriverSlot {
    id: String,
    run_id: String,
    stop: CancellationToken,
    nudge: Arc<Notify>,
    foreground: Arc<ForegroundSlot>,
}
#[derive(Clone)]
struct LiveShell {
    run_id: String,
    handle: ShellHandle,
}

pub struct Control {
    pub store: Store,
    pub attachments: Arc<crate::attachments::AttachmentService>,
    pub index: Arc<crate::index::IndexService>,
    pub gc: Arc<crate::gc::GcService>,
    maintenance: tokio::sync::OnceCell<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    index_workers: tokio::sync::OnceCell<Mutex<Option<crate::index::IndexWorkers>>>,
    shutdown_context: tokio::sync::OnceCell<Result<(), ApiError>>,
    compactor: crate::compaction::Compactor,
    compact_tasks: Mutex<HashMap<String, CancellationToken>>,
    boundaries: Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
    shell: ShellSupervisor,
    instructions: InstructionLoader,
    replay: replay::ReplayCache,
    data_dir: PathBuf,
    #[cfg(unix)]
    endpoint: Arc<std::sync::RwLock<Option<crate::app::Endpoint>>>,
    live: Arc<LiveRegistry>,
    origins: Mutex<HashMap<String, Origin>>,
    events: broadcast::Sender<EngineEvent>,
    changed: Notify,
    stop: CancellationToken,
    diagnostics: crate::diagnostics::Diagnostics,
    storage_failed: AtomicBool,
    active_requests: AtomicUsize,
    context_requests: AtomicUsize,
    admission_closed: AtomicBool,
    waits: wait::WaitRegistry,
    runtime_operations: Mutex<HashMap<String, RuntimeOperation>>,
}
struct RequestGuard {
    control: Arc<Control>,
    changes_context: bool,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.control.active_requests.fetch_sub(1, Ordering::AcqRel);
        if self.changes_context {
            self.control.context_requests.fetch_sub(1, Ordering::SeqCst);
        }
        self.control.changed.notify_waiters();
    }
}
impl Control {
    pub fn new(owner: &DataOwner, startup_cwd: PathBuf) -> Arc<Self> {
        let config = Arc::new(owner.loaded.config.clone());
        let (events, _) = broadcast::channel(256);
        let store = Store::new(
            owner.db.clone(),
            config.clone(),
            startup_cwd,
            owner.instance_id.clone(),
        );
        let index = store.index.clone();
        let compactor = crate::compaction::Compactor::with_clients(
            owner.db.clone(),
            config.clone(),
            store.clients.clone(),
        );
        let attachments = Arc::new(crate::attachments::AttachmentService::new(
            owner.db.clone(),
            &config,
            owner.loaded.data_dir.clone(),
            owner.instance_id.clone(),
        ));
        let gc = Arc::new(crate::gc::GcService::new(
            owner.db.clone(),
            config.clone(),
            owner.loaded.data_dir.clone(),
            attachments.clone(),
        ));
        Arc::new(Self {
            store,
            attachments,
            gc,
            maintenance: tokio::sync::OnceCell::new(),
            index,
            index_workers: tokio::sync::OnceCell::new(),
            shutdown_context: tokio::sync::OnceCell::new(),
            compactor,
            boundaries: Mutex::new(HashMap::new()),
            compact_tasks: Mutex::new(HashMap::new()),
            shell: ShellSupervisor::new(&config),
            instructions: InstructionLoader::new(&config),
            replay: replay::ReplayCache::new(config.cache.history_bytes),
            data_dir: owner.loaded.data_dir.clone(),
            #[cfg(unix)]
            endpoint: owner.endpoint.clone(),
            live: Arc::new(LiveRegistry::default()),
            origins: Mutex::new(HashMap::new()),
            events,
            changed: Notify::new(),
            stop: CancellationToken::new(),
            diagnostics: owner.diagnostics.clone(),
            storage_failed: AtomicBool::new(false),
            active_requests: AtomicUsize::new(0),
            context_requests: AtomicUsize::new(0),
            admission_closed: AtomicBool::new(false),
            waits: wait::WaitRegistry::default(),
            runtime_operations: Mutex::new(HashMap::new()),
        })
    }
    fn admit(self: &Arc<Self>, changes_context: bool) -> Result<RequestGuard, ApiError> {
        // Close and the context count share a SeqCst order: either this guard is
        // counted before capture, or the second close check rejects admission.
        // Acquire/Release alone on the two atomics permits store buffering.
        if self.stop.is_cancelled() || self.admission_closed.load(Ordering::SeqCst) {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        self.active_requests.fetch_add(1, Ordering::AcqRel);
        if changes_context {
            self.context_requests.fetch_add(1, Ordering::SeqCst);
        }
        let guard = RequestGuard {
            control: self.clone(),
            changes_context,
        };
        if self.stop.is_cancelled() || self.admission_closed.load(Ordering::SeqCst) {
            drop(guard);
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        Ok(guard)
    }
    pub async fn start(self: &Arc<Self>) -> Result<(), ApiError> {
        if self.stop.is_cancelled() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        self.index_workers
            .get_or_try_init(|| async {
                Ok::<_, ApiError>(Mutex::new(Some(self.index.start().await?)))
            })
            .await?;
        self.maintenance.get_or_init(||async{
            let gc=self.gc.clone();let attachments=self.attachments.clone();let stop=self.stop.clone();let control=Arc::downgrade(self);
            Mutex::new(Some(tokio::spawn(async move{
                if let Err(error)=attachments.restart_cleanup().await && error.is_storage_failure(){if let Some(control)=control.upgrade(){control.fail_storage().await;}return;}
                loop{
                    if let Some(control)=control.upgrade()
                        && let Err(error)=control.store.collect_receipts().await
                        && error.is_storage_failure() {control.fail_storage().await;break;}
                    tokio::select!{biased;_=stop.cancelled()=>break,result=gc.collect(StorageCategory::All,chrono::Utc::now())=>{if let Err(error)=result && error.is_storage_failure(){if let Some(control)=control.upgrade(){control.fail_storage().await;}break;}}}
                    tokio::select!{biased;_=stop.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(60))=>{}}
                }
            })))
        }).await;
        Ok(())
    }
    /// Read-only accounting of retained conversion buffers and in-flight pins.
    pub fn replay_cache_stats(&self) -> replay::ReplayStats {
        self.replay.stats()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }
    fn changed(&self, thread_id: &str) {
        self.changed_with_recovery(thread_id, false);
    }
    fn changed_with_recovery(&self, thread_id: &str, recovery: bool) {
        let _ = self.events.send(EngineEvent::Changed {
            thread_id: thread_id.into(),
            recovery,
        });
        self.changed.notify_waiters();
    }

    /// This task never waits for a model or owned shell to finish. Individual
    /// requests may await DB/network-independent observations without occupying it.
    pub async fn serve(self: Arc<Self>, mut requests: mpsc::Receiver<Invocation>) {
        while let Some(invocation) = requests.recv().await {
            let control = self.clone();
            tokio::spawn(async move {
                // Capture the admission field explicitly: disjoint closure capture
                // must not drop it with the transport/dispatcher iteration.
                let _admission = invocation.admission;
                let _guard = match control.admit(invocation.request.command.changes_context()) {
                    Ok(guard) => guard,
                    Err(error) => {
                        let _ = invocation
                            .reply
                            .send(Response::failure(invocation.request.operation_id, error));
                        return;
                    }
                };
                let (reply, effects) = control
                    .evaluate(invocation.request, invocation.disconnected)
                    .await;
                let _ = invocation.reply.send(reply);
                // A self-stop receipt is handed to the transport before killing
                // its caller. The finite grace also handles disconnected clients.
                control.apply_effects(effects).await;
            });
        }
    }
    pub async fn request(self: &Arc<Self>, request: Request) -> Response<Value> {
        self.request_cancellable(request, CancellationToken::new())
            .await
    }
    /// Cancel a caller's observation (not its target or accepted mutations).
    /// The owned worker keeps admission and durable result finalization alive.
    pub async fn request_cancellable(
        self: &Arc<Self>,
        request: Request,
        disconnected: CancellationToken,
    ) -> Response<Value> {
        let control = self.clone();
        let operation = request.operation_id.clone();
        tokio::spawn(async move {
            let _guard = match control.admit(request.command.changes_context()) {
                Ok(guard) => guard,
                Err(error) => return Response::failure(request.operation_id, error),
            };
            let (reply, effects) = control.evaluate(request, disconnected).await;
            control.apply_effects(effects).await;
            reply
        })
        .await
        .unwrap_or_else(|_| {
            Response::failure(
                operation,
                ApiError::new(
                    ErrorCode::InvalidState,
                    "Operation worker failed; inspect its receipt before retrying",
                ),
            )
        })
    }
    /// Re-read current attachment bytes without sending a model request or
    /// changing the last successful-send baseline/selection.
    pub async fn reload_attachments(
        self: &Arc<Self>,
        thread_id: String,
    ) -> Result<Value, ApiError> {
        let control = self.clone();
        tokio::spawn(async move {
            let _guard = control.admit(true)?;
            let prepared = control
                .attachments
                .prepare(&thread_id, control.stop.child_token(), true)
                .await?;
            Ok(serde_json::json!({
                "thread_id": prepared.thread_id,
                "selection_revision": prepared.selection_revision,
                "attachments": prepared.manifest,
                "notices": prepared.notices,
                "would_exclude": prepared.excluded_ids,
                "sent": false
            }))
        })
        .await
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Attachment reload worker failed"))?
    }
    pub async fn send_new_thread(
        self: &Arc<Self>,
        operation_id: String,
        args: ThreadCreateArgs,
        text: String,
    ) -> Result<Value, ApiError> {
        let control = self.clone();
        tokio::spawn(async move {
            let _guard = control.admit(true)?;
            control
                .send_new_thread_inner(operation_id, args, text)
                .await
        })
        .await
        .map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Send worker failed; inspect its operation receipt",
            )
        })?
    }
    async fn send_new_thread_inner(
        self: &Arc<Self>,
        operation_id: String,
        args: ThreadCreateArgs,
        text: String,
    ) -> Result<Value, ApiError> {
        if self.stop.is_cancelled() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        match self.store.send_new_thread(operation_id, args, text).await {
            Ok(effects) => {
                let result = effects.result.clone();
                self.apply_effects(Some(effects)).await;
                Ok(result)
            }
            Err(error) => {
                if error.is_storage_failure() {
                    self.fail_storage().await;
                }
                Err(error)
            }
        }
    }
    pub async fn edit_request(
        self: &Arc<Self>,
        operation_id: String,
        thread: String,
        node: String,
        confirmed_run: Option<String>,
        text: String,
        files: Vec<crate::attachments::AttachmentInput>,
    ) -> Result<Value, ApiError> {
        self.edit_request_with_settings(
            operation_id,
            thread,
            node,
            confirmed_run,
            text,
            files,
            ThreadCreateArgs {
                connection: None,
                model: None,
                effort: None,
                cwd: None,
                parent_id: None,
            },
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn edit_request_with_settings(
        self: &Arc<Self>,
        operation_id: String,
        thread: String,
        node: String,
        confirmed_run: Option<String>,
        text: String,
        files: Vec<crate::attachments::AttachmentInput>,
        args: ThreadCreateArgs,
    ) -> Result<Value, ApiError> {
        let content = crate::attachments::DraftContent::appended(text, files.len());
        self.edit_ordered_request(
            operation_id,
            thread,
            node,
            confirmed_run,
            content,
            files,
            args,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn edit_ordered_request(
        self: &Arc<Self>,
        operation_id: String,
        thread: String,
        node: String,
        confirmed_run: Option<String>,
        content: crate::attachments::DraftContent,
        files: Vec<crate::attachments::AttachmentInput>,
        args: ThreadCreateArgs,
    ) -> Result<Value, ApiError> {
        content.validate(files.len())?;

        let control = self.clone();
        let outcome=tokio::spawn(async move{
            let _guard=control.admit(true)?;
            let references:Vec<Value>=files.iter().map(|file|json!({"path":file.path,"kind":file.kind})).collect();
            let mut intent=json!({"kind":"edit_request","thread_id":thread,"node_id":node,"confirmed_run_id":confirmed_run,"text":content.text,"files":references,"args":args});
            content.fingerprint_positions(&mut intent);
            let reservation=control.store.reserve_internal(operation_id.clone(),intent).await?;
            match reservation{db::RuntimeReservation::Saved(result)=>return result,db::RuntimeReservation::Pending=>return Err(ApiError::new(ErrorCode::InvalidState,"This edit was accepted but its result is uncertain; inspect its operation receipt")),db::RuntimeReservation::New=>{}}
            let outcome=async{
                let mut effects=control.store.stop_for_edit(thread.clone(),confirmed_run).await?;
                // This in-process edit has no shell caller awaiting a stop
                // receipt. Apply its stop immediately; the accepted edit still
                // owns its guard until the terminal boundary and durable fork.
                let runs=std::mem::take(&mut effects.stop_runs);
                control.apply_effects(Some(effects)).await;
                for run in &runs { control.stop_run(run).await; }
                for run in runs{
                    let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(30_000);
                    loop{
                        let run=run.clone();let notified=control.changed.notified();tokio::pin!(notified);notified.as_mut().enable();
                        let terminal=control.store.database.call(move|db|Ok(db.query_row("SELECT state NOT IN ('live','stopping') FROM run WHERE id=?1",[run],|r|r.get::<_,bool>(0))?)).await.map_err(storage_error)?;
                        if terminal{break;}
                        tokio::select!{biased;_=control.stop.cancelled()=>return Err(ApiError::new(ErrorCode::InvalidState,"Edit cancelled by shutdown; the original request remains")),_=tokio::time::sleep_until(deadline)=>return Err(ApiError::new(ErrorCode::InvalidState,"The old Run is still stopping; inspect it and retry the retained draft")),_=notified=>{}}
                    }
                }
                let target=thread.clone();let inherited_cwd=control.store.database.call(move|db|{use rusqlite::OptionalExtension;Ok(db.query_row("SELECT cwd FROM thread WHERE id=?1 AND deleted_at IS NULL",[target],|r|r.get::<_,String>(0)).optional()?)}).await.map_err(storage_error)?.ok_or_else(||ApiError::new(ErrorCode::ThreadNotFound,"Edited Thread was not found or was deleted"))?;
                let base=PathBuf::from(inherited_cwd);let cwd=args.cwd.as_ref().map(PathBuf::from).map(|path|if path.is_absolute(){path}else{base.join(path)}).unwrap_or(base);
                let file_limit=control.store.config.limits.attachment_file_bytes;let request_limit=control.store.config.limits.attachment_request_bytes;
                let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(control.store.config.io.file_prepare_timeout_ms);
                let files=control.attachments.workers.execute(deadline,control.stop.child_token(),move||{
                    let mut planned=Vec::new();let mut total=0u64;
                    for file in files{let path=if file.path.is_absolute(){file.path}else{cwd.join(file.path)};if path.to_str().is_none(){return Err(ApiError::new(ErrorCode::InvalidArgument,"Attachment path must be Unicode"));}let observed_bytes=std::fs::metadata(&path).ok().filter(|m|m.is_file()).map(|m|m.len());total=total.saturating_add(observed_bytes.filter(|bytes|*bytes<=file_limit).unwrap_or(0));planned.push(crate::attachments::PlannedAttachment{path,kind:file.kind,observed_bytes,managed_id:None});}
                    if total>request_limit{return Err(ApiError::new(ErrorCode::InputLimit,"Edited draft protected attachments exceed the request limit"));}Ok(planned)
                }).await?;
                if control.stop.is_cancelled(){return Err(ApiError::new(ErrorCode::InvalidState,"Edit cancelled by shutdown; draft retained"));}
                let boundary=control.boundary(&thread).await;
                let _boundary=tokio::select!{biased;_=control.stop.cancelled()=>return Err(ApiError::new(ErrorCode::InvalidState,"Edit cancelled by shutdown")),guard=boundary.lock()=>guard};
                let effects=control.store.accept_ordered_edit(operation_id.clone(),thread,node,content,files,args).await?;
                let value=effects.result.clone();control.apply_effects(Some(effects)).await;Ok(value)
            }.await;
            control.store.finalize_internal(operation_id,outcome.clone()).await?;
            if outcome.as_ref().err().is_some_and(|error|error.is_storage_failure()){control.fail_storage().await;}
            outcome
        }).await.map_err(|_|ApiError::new(ErrorCode::InvalidState,"Edit worker failed; inspect its receipt"))?;
        if outcome
            .as_ref()
            .err()
            .is_some_and(|error| error.is_storage_failure())
        {
            self.fail_storage().await;
        }
        outcome
    }
    async fn boundary(&self, thread: &str) -> Arc<Mutex<()>> {
        let mut boundaries = self.boundaries.lock().await;
        boundaries.retain(|_, value| value.strong_count() > 0);
        if let Some(boundary) = boundaries.get(thread).and_then(std::sync::Weak::upgrade) {
            return boundary;
        }
        let boundary = Arc::new(Mutex::new(()));
        boundaries.insert(thread.into(), Arc::downgrade(&boundary));
        boundary
    }
    pub async fn send_draft(
        self: &Arc<Self>,
        operation_id: String,
        args: ThreadCreateArgs,
        thread_id: Option<String>,
        text: String,
        files: Vec<crate::attachments::AttachmentInput>,
    ) -> Result<Value, ApiError> {
        let content = crate::attachments::DraftContent::appended(text, files.len());
        self.send_ordered_draft(operation_id, args, thread_id, content, files)
            .await
    }
    pub(crate) async fn send_ordered_draft(
        self: &Arc<Self>,
        operation_id: String,
        args: ThreadCreateArgs,
        thread_id: Option<String>,
        content: crate::attachments::DraftContent,
        files: Vec<crate::attachments::AttachmentInput>,
    ) -> Result<Value, ApiError> {
        content.validate(files.len())?;

        let control = self.clone();
        tokio::spawn(async move {
            let _guard = control.admit(true)?;
            control
                .send_draft_inner(operation_id, args, thread_id, content, files)
                .await
        })
        .await
        .map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Send worker failed; inspect its operation receipt",
            )
        })?
    }
    async fn send_draft_inner(
        self: &Arc<Self>,
        operation_id: String,
        args: ThreadCreateArgs,
        thread_id: Option<String>,
        content: crate::attachments::DraftContent,
        files: Vec<crate::attachments::AttachmentInput>,
    ) -> Result<Value, ApiError> {
        if self.stop.is_cancelled() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Owner is shutting down",
            ));
        }
        self.start().await?;
        let (files, revision) = if let Some(thread) = &thread_id {
            let plan = self
                .attachments
                .plan_add(thread, files, self.stop.child_token())
                .await?;
            (plan.files, Some(plan.selection_revision))
        } else {
            let cwd = args
                .cwd
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.store.startup_cwd.clone());
            let cwd = if cwd.is_absolute() {
                cwd
            } else {
                self.store.startup_cwd.join(cwd)
            };
            let file_limit = self.store.config.limits.attachment_file_bytes;
            let request_limit = self.store.config.limits.attachment_request_bytes;
            let deadline = tokio::time::Instant::now()
                + std::time::Duration::from_millis(self.store.config.io.file_prepare_timeout_ms);
            let files=self.attachments.workers.execute(deadline,self.stop.child_token(),move||{
                let mut planned=Vec::new();let mut total=0u64;
                for file in files {
                    let path=if file.path.is_absolute(){file.path}else{cwd.join(file.path)};
                    if path.to_str().is_none(){return Err(ApiError::new(ErrorCode::InvalidArgument,"Attachment path must be valid Unicode"));}
                    let observed_bytes=std::fs::metadata(&path).ok().filter(|m|m.is_file()).map(|m|m.len());
                    total=total.saturating_add(observed_bytes.filter(|v|*v<=file_limit).unwrap_or(0));
                    planned.push(crate::attachments::PlannedAttachment{path,kind:file.kind,observed_bytes,managed_id:None});
                }
                if total>request_limit{return Err(ApiError::new(ErrorCode::InputLimit,"Protected draft attachments exceed the request raw-byte limit; reduce or split them"));}
                Ok(planned)
            }).await?;
            (files, None)
        };
        match self
            .store
            .send_draft_content(operation_id, args, thread_id, content, files, revision)
            .await
        {
            Ok(effects) => {
                let result = effects.result.clone();
                self.apply_effects(Some(effects)).await;
                Ok(result)
            }
            Err(error) => {
                if error.is_storage_failure() {
                    self.fail_storage().await;
                }
                Err(error)
            }
        }
    }

    async fn validate_origin(&self, origin: &Option<Origin>) -> Result<Option<Origin>, ApiError> {
        match origin {
            None => Ok(None),
            Some(origin) => {
                let issued = self.origins.lock().await;
                if issued.get(&origin.token) != Some(origin)
                    || origin.instance_id != self.store.instance_id
                {
                    Err(ApiError::new(
                        ErrorCode::InvalidArgument,
                        "Origin was not issued by this owner instance",
                    ))
                } else {
                    Ok(Some(origin.clone()))
                }
            }
        }
    }
    async fn evaluate(
        self: &Arc<Self>,
        request: Request,
        disconnected: CancellationToken,
    ) -> (Response<Value>, Option<MutationResult>) {
        let operation = request.operation_id.clone();
        let observation = !request.command.requires_receipt();
        let observation_name = request.command.name();
        let result = async {
            request.validate()?;
            let origin = self.validate_origin(&request.origin).await?;
            self.start().await?;
            if self.stop.is_cancelled() {
                return Err(ApiError::new(
                    ErrorCode::InvalidState,
                    "Owner is shutting down",
                ));
            }
            match request.command.route() {
                CommandRoute::Runtime => {
                    let result = if observation {
                        self.runtime_observation(request, origin, disconnected)
                            .await
                    } else {
                        self.runtime_request(request, origin, disconnected).await
                    };
                    result.map(|result| MutationResult {
                        result,
                        ..MutationResult::default()
                    })
                }
                CommandRoute::Database | CommandRoute::StatusProjection => match &request.command {
                    Command::AttachmentAdd(args) => {
                        if let Some(saved) = self.store.replay(&request, &origin).await? {
                            return Ok(saved);
                        }
                        let thread = args
                            .thread_id
                            .clone()
                            .or_else(|| origin.as_ref().map(|o| o.thread_id.clone()))
                            .ok_or_else(|| {
                                ApiError::new(
                                    ErrorCode::InvalidArgument,
                                    "Target Thread is required",
                                )
                            })?;
                        let plan = self
                            .attachments
                            .plan_add(
                                &thread,
                                vec![crate::attachments::AttachmentInput {
                                    path: PathBuf::from(&args.path),
                                    kind: args.kind,
                                }],
                                self.stop.child_token(),
                            )
                            .await?;
                        self.store.execute_with_plan(request, origin, plan).await
                    }
                    _ => self.store.execute(request, origin).await,
                },
            }
        }
        .await;
        // Receipt-free results still obey the exact response-envelope bound,
        // including operation ID and recovery hints, before reaching transport.
        let result = if observation {
            let result = result.map_err(|error: ApiError| {
                if error.hint.is_none()
                    && matches!(error.code, ErrorCode::IoFailed | ErrorCode::InvalidState)
                {
                    let cost = if observation_name == "history.search" {
                        " Repeating semantic search may incur another query embedding charge."
                    } else { "" };
                    error.hint(format!("This observation did not create an operation receipt. Check the reported cause, then repeat shellboy {} for fresh state.{cost}", observation_name.replace('.', " ")))
                } else { error }
            });
            let projected = result
                .as_ref()
                .map(|effects| effects.result.clone())
                .map_err(Clone::clone);
            match receipt::encode(&operation, &projected) {
                Ok(_) => result,
                Err(error) => Err(error),
            }
        } else {
            result
        };
        match result {
            Ok(effects) => (
                Response::success(operation, effects.result.clone()),
                Some(effects),
            ),
            Err(error) => {
                if error.is_storage_failure() {
                    self.fail_storage().await;
                }
                (Response::failure(operation, error), None)
            }
        }
    }
    /// UI actions have no public operation ID. Keep human stop available when
    /// ordinary receipt admission is full; the canonical reducer still owns
    /// state, generation, held delivery, and descendant-stop semantics.
    pub(crate) async fn cancel_thread(self: &Arc<Self>, thread: String) -> Result<(), ApiError> {
        let _guard = self.admit(true)?;
        self.start().await?;
        match self.store.stop_current_thread(thread).await {
            Ok(effects) => {
                self.apply_effects(Some(effects)).await;
                Ok(())
            }
            Err(error) => {
                if error.is_storage_failure() {
                    self.fail_storage().await;
                }
                Err(error)
            }
        }
    }

    async fn apply_effects(self: &Arc<Self>, effects: Option<MutationResult>) {
        let Some(effects) = effects else {
            return;
        };
        self.index.wake();
        for thread in effects.changed_threads {
            self.changed(&thread);
        }
        for thread in effects.compact_threads {
            self.queue_compact(thread).await;
        }
        if !effects.stop_runs.is_empty() {
            self.reply_grace().await;
            for run_id in effects.stop_runs {
                self.stop_run(&run_id).await;
            }
        }
        for thread in effects.wake_threads {
            self.changed(&thread);
            self.kick(thread).await;
        }
        for thread in effects.quiet_wake_threads {
            self.changed(&thread);
            self.kick_quiet(thread).await;
        }
    }
    fn queue_compact(
        self: &Arc<Self>,
        thread: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let control = self.clone();
        Box::pin(async move {
            if control.stop.is_cancelled() {
                return;
            }
            let mut tasks = control.compact_tasks.lock().await;
            if tasks.contains_key(&thread) {
                return;
            }
            let token = control.stop.child_token();
            tasks.insert(thread.clone(), token.clone());
            drop(tasks);
            tokio::spawn(async move {
                let boundary = control.boundary(&thread).await;
                loop {
                    let guard = tokio::select! {biased;_=token.cancelled()=>None,guard=boundary.lock()=>Some(guard)};
                    let target = thread.clone();
                    let operations = control
                        .store
                        .database
                        .call(move |db| {
                            use rusqlite::OptionalExtension;
                            let tx = db.transaction()?;
                            let key = crate::compaction::CompactionKeys::new(&target).pending();
                            let body: Option<String> = tx
                                .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |r| {
                                    r.get(0)
                                })
                                .optional()?;
                            tx.execute("DELETE FROM app_meta WHERE key=?1", [key])?;
                            if let Some(body) = &body {
                                tx.execute(
                                    "INSERT OR REPLACE INTO app_meta(key,value)VALUES(?1,?2)",
                                    rusqlite::params![
                                        crate::compaction::CompactionKeys::new(&target).inflight(),
                                        body
                                    ],
                                )?;
                            }
                            tx.commit()?;
                            Ok(body
                                .and_then(|body| serde_json::from_str::<Vec<String>>(&body).ok())
                                .unwrap_or_default())
                        })
                        .await;
                    let operations = match operations {
                        Ok(value) => value,
                        Err(_) => {
                            control.fail_storage().await;
                            break;
                        }
                    };
                    if operations.is_empty() {
                        break;
                    }
                    let result = if guard.is_none() {
                        Err(ApiError::new(
                            ErrorCode::InvalidState,
                            "Queued compaction cancelled by stop",
                        ))
                    } else if let Err(error) = control.gc.require_capacity().await {
                        Err(error)
                    } else {
                        match control
                            .compactor
                            .spill_cancellable(&thread, false, token.clone())
                            .await
                        {
                            Err(error) => Err(error),
                            Ok(spill) => match control
                                .compactor
                                .summarize(&thread, false, token.clone())
                                .await
                            {
                                Err(error) => Err(error),
                                Ok(mut summary) => {
                                    summary.spilled_groups += spill.spilled_groups;
                                    summary.source_nodes += spill.source_nodes;
                                    summary.original_node_ids.extend(spill.original_node_ids);
                                    summary.no_target = summary.no_target && spill.no_target;
                                    encode_result(
                                        summary,
                                        ErrorCode::InvalidState,
                                        "Cannot encode compact result",
                                    )
                                }
                            },
                        }
                    };
                    let outcome = match &result {
                        Ok(value) => json!({"result":value}),
                        Err(error) => json!({"error":error}),
                    };
                    let inflight = crate::compaction::CompactionKeys::new(&thread).inflight();
                    let save_failed = result
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.is_storage_failure());
                    let saved=control.store.database.call(move|db|{let tx=db.transaction()?;tx.execute("DELETE FROM app_meta WHERE key=?1",[inflight])?;for operation in operations{let downstream=json!({"kind":"compact","state":if result.is_ok(){"completed"}else{"failed"},"outcome":outcome,"ended_at":chrono::Utc::now().to_rfc3339()});tx.execute("INSERT OR REPLACE INTO app_meta(key,value) VALUES(?1,?2)",rusqlite::params![crate::compaction::CompactionKeys::result(&operation),downstream.to_string()])?;}tx.commit()?;Ok(())}).await;
                    if saved.is_err() || save_failed {
                        control.fail_storage().await;
                        break;
                    }
                    drop(guard);
                    control.changed(&thread);
                    if token.is_cancelled() {
                        break;
                    }
                }
                control.compact_tasks.lock().await.remove(&thread);
                control.changed.notify_waiters();
                if !control.stop.is_cancelled() {
                    let key = crate::compaction::CompactionKeys::new(&thread).pending();
                    if matches!(
                        control
                            .store
                            .database
                            .call(move |db| Ok(db.query_row(
                                "SELECT EXISTS(SELECT 1 FROM app_meta WHERE key=?1)",
                                [key],
                                |r| r.get::<_, bool>(0)
                            )?))
                            .await,
                        Ok(true)
                    ) {
                        control.queue_compact(thread).await;
                    }
                }
            });
        })
    }

    async fn stop_run(&self, run_id: &str) {
        let run = run_id.to_owned();
        let thread = self
            .store
            .database
            .call(move |db| {
                Ok(
                    db.query_row("SELECT thread_id FROM run WHERE id=?1", [run], |r| {
                        r.get::<_, String>(0)
                    })?,
                )
            })
            .await;
        if let Ok(thread) = thread {
            if let Some(token) = self.compact_tasks.lock().await.get(&thread) {
                token.cancel();
            }
            let active = self.live.cancel_run(&thread, run_id).await;
            if !active && let Err(error) = driver::stop_without_driver(self, run_id).await {
                self.fail_storage().await;
                let _ = self.events.send(EngineEvent::Error {
                    thread_id: thread.clone(),
                    error,
                });
            }
            self.changed(&thread);
        }
    }

    pub(crate) async fn fail_storage(&self) {
        self.diagnostics.record(
            crate::diagnostics::DiagnosticEvent::DatabaseFailure,
            crate::diagnostics::DiagnosticIds::default(),
        );
        self.storage_failed.store(true, Ordering::Release);
        self.begin_shutdown();
        self.index.cancel();
        // A read observer may disappear after recognizing the failure. Retain
        // shell-stop work independently; orderly shutdown later joins collectors
        // and index workers before closing the DB.
        let live = self.live.clone();
        let stop = tokio::spawn(async move {
            live.stop_shells().await;
        });
        let _ = stop.await;
    }
    async fn nudge_owner(self: &Arc<Self>, child_run: &str) {
        let run = child_run.to_owned();
        let owner=self.store.database.call(move|db| {
            use rusqlite::OptionalExtension;
            Ok(db.query_row("SELECT parent.thread_id FROM run child JOIN run parent ON parent.id=child.owner_run_id WHERE child.id=?1 AND parent.state='live'",[run],|row|row.get::<_,String>(0)).optional()?)
        }).await;
        match owner {
            Ok(Some(thread)) => self.kick_quiet(thread).await,
            Err(_) => self.fail_storage().await,
            _ => {}
        }
    }
    pub async fn background(&self, thread_id: &str) -> Result<(), ApiError> {
        self.live.background(thread_id).await
    }
    fn kick(
        self: &Arc<Self>,
        thread_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        self.kick_inner(thread_id, true)
    }

    fn kick_quiet(
        self: &Arc<Self>,
        thread_id: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        self.kick_inner(thread_id, false)
    }

    fn kick_inner(
        self: &Arc<Self>,
        thread_id: String,
        interrupt: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let control = self.clone();
        Box::pin(async move {
            if control.stop.is_cancelled() {
                return;
            }
            let target_thread = thread_id.clone();
            let run = control
                .store
                .database
                .call(move |db| {
                    use rusqlite::OptionalExtension;
                    Ok(db
                        .query_row(
                            "SELECT id FROM run WHERE thread_id=?1 AND state='live'",
                            [target_thread],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?)
                })
                .await;
            let run_id = match run {
                Ok(Some(run)) => run,
                Ok(None) => return,
                Err(_) => {
                    control.fail_storage().await;
                    return;
                }
            };
            let Some(slot) = control
                .live
                .register_or_nudge_driver(&thread_id, run_id, &control.stop, interrupt)
                .await
            else {
                return;
            };
            tokio::spawn(async move {
                if let Err(error) =
                    driver::drive(control.clone(), thread_id.clone(), slot.clone()).await
                {
                    if error.is_storage_failure() {
                        control.fail_storage().await;
                    }
                    let _ = control.events.send(EngineEvent::Error {
                        thread_id: thread_id.clone(),
                        error,
                    });
                }
                control
                    .live
                    .remove_driver_if_current(&thread_id, &slot.id)
                    .await;
                control.changed(&thread_id);
                control.nudge_owner(&slot.run_id).await;
                // An accepted send/retry can race the previous driver's final
                // observation and map removal. Recheck after releasing its slot.
                if !control.stop.is_cancelled() {
                    let thread = thread_id.clone();
                    let pending=control.store.database.call(move|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM turn t JOIN run r ON r.id=t.run_id WHERE t.thread_id=?1 AND r.state='live' AND t.state IN ('preparing','running','waiting'))",[thread],|row|row.get::<_,bool>(0))?)).await;
                    match pending {
                        Ok(true) => control.kick_quiet(thread_id).await,
                        Err(_) => control.fail_storage().await,
                        _ => {}
                    }
                }
            });
        })
    }
    /// Close admission immediately while the adapter preserves its drafts and
    /// drains already accepted work. This method never waits for a lock or I/O.
    pub fn begin_shutdown(&self) {
        self.shell.close_start_admission();
        self.admission_closed.store(true, Ordering::SeqCst);
        self.stop.cancel();
        self.live.try_stop_shells();
        self.changed.notify_waiters();
    }
    /// Capture before cancellation releases waits or terminalizes owned Runs.
    /// A failed snapshot must still cancel/drain; shutdown propagates this result.
    pub async fn prepare_shutdown(&self) -> Result<(), ApiError> {
        self.shell.close_start_admission();
        self.admission_closed.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
        let result = self
            .shutdown_context
            .get_or_init(|| async {
                // Unlike waits and long reads, these admitted workers can still
                // create Runs or sources after file preparation. Let them reach
                // their saved outcome before taking the pre-cancellation view.
                loop {
                    let notified = self.changed.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if self.context_requests.load(Ordering::SeqCst) == 0 {
                        break;
                    }
                    notified.await;
                }
                self.store
                    .database
                    .call(|db| {
                        let tx = db.transaction()?;
                        interruption::capture_interrupted_context(
                            &tx,
                            &chrono::Utc::now().to_rfc3339(),
                            "app_shutdown",
                            "shutdown",
                        )?;
                        tx.commit()?;
                        Ok(())
                    })
                    .await
                    .map_err(storage_error)
            })
            .await
            .clone();
        if result
            .as_ref()
            .err()
            .is_some_and(ApiError::is_storage_failure)
        {
            self.fail_storage().await;
        }
        self.begin_shutdown();
        result
    }
    pub(crate) fn tool_start_gate(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.shell.start_gate()
    }
    pub(crate) fn close_tool_start_admission(&self) {
        self.shell.close_start_admission();
    }
    async fn work_is_drained(&self) -> bool {
        // End each guard's scope before acquiring the next ownership map.
        let live_empty = self.live.is_empty().await;
        let operations_empty = self.runtime_operations.lock().await.is_empty();
        let compactions_empty = self.compact_tasks.lock().await.is_empty();
        live_empty
            && operations_empty
            && compactions_empty
            && self.active_requests.load(Ordering::Acquire) == 0
    }

    pub async fn shutdown(self: &Arc<Self>) -> Result<(), ApiError> {
        let context_result = self.prepare_shutdown().await;
        if let Some(worker) = self.maintenance.get()
            && let Some(worker) = worker.lock().await.take()
        {
            let _ = worker.await;
        }

        if let Some(workers) = self.index_workers.get()
            && let Some(workers) = workers.lock().await.take()
        {
            workers.shutdown().await;
        }
        self.live.stop_shells().await;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.work_is_drained().await {
                break;
            }
            notified.await;
        }
        // An admitted startup may have published workers while shutdown first
        // inspected its OnceCell. Drain any such late publication as well.
        if let Some(worker) = self.maintenance.get()
            && let Some(worker) = worker.lock().await.take()
        {
            let _ = worker.await;
        }
        if let Some(workers) = self.index_workers.get()
            && let Some(workers) = workers.lock().await.take()
        {
            workers.shutdown().await;
        }
        tokio::try_join!(
            self.attachments.wait_idle(),
            self.instructions.wait_idle(),
            self.gc.wait_idle(),
            self.index.wait_idle()
        )?;
        // Owner releases the lock only after this and DbWorker::shutdown.
        self.store
            .database
            .call(|db| {
                let tx = db.transaction()?;
                state::finish_drained_shutdown(&tx)?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(storage_error)?;
        context_result?;
        if self.storage_failed.load(Ordering::Acquire) {
            return Err(ApiError::new(
                ErrorCode::SaveFailed,
                "A persistence failure stopped execution; shutdown completed with unsaved source data",
            ));
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod failure_tests {
    use super::*;
    use std::{collections::BTreeMap, time::Duration};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_closes_admission_but_finishes_file_preparation_before_capture() {
        for save_failure in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let config = root.path().join("config.toml");
            std::fs::write(&config, "default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
            let owner = DataOwner::start(
                Some(config),
                Some(root.path().join("data")),
                root.path().into(),
            )
            .await
            .unwrap();
            let control = Control::new(&owner, root.path().into());
            if save_failure {
                owner.db.call(|db| {db.execute_batch("CREATE TRIGGER reject_send BEFORE INSERT ON thread BEGIN SELECT RAISE(ABORT,'fixture mutation save failure'); END;")?;Ok(())}).await.unwrap();
            }
            // Occupy every real finite file worker, with an acknowledged barrier
            // rather than a scheduling delay. The normal send must wait here.
            let mut occupied = Vec::new();
            let mut releases = Vec::new();
            for _ in 0..control.attachments.workers.available() {
                let workers = control.attachments.workers.clone();
                let (started, ready) = tokio::sync::oneshot::channel();
                let (release, held) = std::sync::mpsc::channel();
                occupied.push(tokio::spawn(async move {
                    workers
                        .execute(
                            tokio::time::Instant::now() + Duration::from_secs(10),
                            CancellationToken::new(),
                            move || {
                                started.send(()).unwrap();
                                held.recv().unwrap();
                                Ok(())
                            },
                        )
                        .await
                }));
                ready.await.unwrap();
                releases.push(release);
            }
            let sending = control.clone();
            let send = tokio::spawn(async move {
                sending
                    .send_draft(
                        format!("{}:barrier-send", sending.store.instance_id),
                        ThreadCreateArgs {
                            connection: None,
                            model: None,
                            effort: None,
                            cwd: None,
                            parent_id: None,
                        },
                        None,
                        "accepted before close".into(),
                        vec![],
                    )
                    .await
            });
            tokio::time::timeout(Duration::from_secs(3), async {
                while control.context_requests.load(Ordering::Acquire) != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let preparing = control.clone();
            let prepare = tokio::spawn(async move { preparing.prepare_shutdown().await });
            tokio::time::timeout(Duration::from_secs(3), async {
                while !control.admission_closed.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!control.stop.is_cancelled());
            assert!(!prepare.is_finished());
            let rejected = control
                .request(Request::new(
                    format!("{}:late", control.store.instance_id),
                    Command::ThreadCreate(ThreadCreateArgs {
                        connection: None,
                        model: None,
                        effort: None,
                        cwd: None,
                        parent_id: None,
                    }),
                ))
                .await;
            assert_eq!(rejected.error.unwrap().code, ErrorCode::InvalidState);
            owner
                .db
                .call(|db| {
                    assert_eq!(
                        db.query_row(
                            "SELECT COUNT(*) FROM node WHERE kind='interruption_context'",
                            [],
                            |r| r.get::<_, i64>(0)
                        )?,
                        0
                    );
                    Ok(())
                })
                .await
                .unwrap();
            for release in releases {
                release.send(()).unwrap();
            }
            for task in occupied {
                task.await.unwrap().unwrap();
            }
            let sent = send.await.unwrap();
            prepare.await.unwrap().unwrap();
            let shutdown = tokio::time::timeout(Duration::from_secs(5), control.shutdown())
                .await
                .unwrap();
            if save_failure {
                assert_eq!(sent.unwrap_err().code, ErrorCode::SaveFailed);
                assert_eq!(shutdown.unwrap_err().code, ErrorCode::SaveFailed);
            } else {
                let accepted = sent.unwrap();
                shutdown.unwrap();
                let thread = accepted["thread_id"].as_str().unwrap().to_owned();
                owner.db.call(move |db| {
                    let snapshot:String=db.query_row("SELECT payload_ref_json FROM event WHERE target_thread_id=?1 AND json_extract(payload_ref_json,'$.change.reason')='app_shutdown'",[&thread],|r|r.get(0))?;
                    assert!(snapshot.contains("app_shutdown"));
                    assert_eq!(db.query_row("SELECT COUNT(*) FROM receipt WHERE target_thread_id=?1",[thread],|r|r.get::<_,i64>(0))?,1);
                    assert_eq!(db.query_row("SELECT COUNT(*) FROM run WHERE state IN ('live','stopping')",[],|r|r.get::<_,i64>(0))?,0);
                    Ok(())
                }).await.unwrap();
            }
            assert_eq!(control.context_requests.load(Ordering::Acquire), 0);
            assert_eq!(control.active_requests.load(Ordering::Acquire), 0);
            owner.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn forced_shutdown_can_interrupt_pre_capture_admission_drain() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let guard = control.admit(true).unwrap();
        let preparing = control.clone();
        let prepare = tokio::spawn(async move { preparing.prepare_shutdown().await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !control.admission_closed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!prepare.is_finished());
        // The UI's Force branch drops the graceful pipeline. Cancellation must
        // release the OnceCell initializer, without waiting for this guard.
        prepare.abort();
        assert!(prepare.await.unwrap_err().is_cancelled());
        control.begin_shutdown();
        assert!(control.stop.is_cancelled());
        assert_eq!(control.context_requests.load(Ordering::Acquire), 1);
        drop(guard);
        tokio::time::timeout(Duration::from_secs(3), control.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(control.active_requests.load(Ordering::Acquire), 0);
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn authenticated_model_catalog_uses_origin_thread_unless_connection_overrides_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
                    head.push(byte[0]);
                }
                requests.push(String::from_utf8(head).unwrap());
                let body = r#"{"data":[{"id":"selected-model"}]}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            requests
        });
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, format!("default_connection=\"first\"\ndefault_model=\"unchanged\"\n[connections.first]\nbase_url=\"http://{address}/first/v1\"\n[connections.second]\nbase_url=\"http://{address}/second/v1\"\n")).unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let req = |command| {
            Request::new(
                format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
                command,
            )
        };
        let created = control
            .request(req(Command::ThreadCreate(ThreadCreateArgs {
                connection: Some("second".into()),
                cwd: None,
                model: None,
                effort: None,
                parent_id: None,
            })))
            .await;
        let origin = Origin {
            instance_id: control.store.instance_id.clone(),
            thread_id: created.result.unwrap()["thread_id"]
                .as_str()
                .unwrap()
                .into(),
            run_id: "issued-run".into(),
            turn_id: "issued-turn".into(),
            execution_id: "issued-shell".into(),
            token: uuid::Uuid::new_v4().to_string(),
        };
        control
            .origins
            .lock()
            .await
            .insert(origin.token.clone(), origin.clone());
        for (connection, expected) in [(None, "second"), (Some("first".to_owned()), "first")] {
            let mut request = req(Command::ModelList(ModelListArgs {
                connection,
                thread_id: None,
            }));
            request.origin = Some(origin.clone());
            let response = control.request(request).await;
            assert!(response.ok, "{:?}", response.error);
            assert_eq!(response.result.unwrap()["connection"], expected);
        }
        let requests = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(requests[0].starts_with("GET /second/v1/models HTTP/1.1"));
        assert!(requests[1].starts_with("GET /first/v1/models HTTP/1.1"));
        control.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_failure_observer_still_stops_a_shell_after_the_map_unlocks() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let owner = DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = Control::new(&owner, root.path().into());
        let args: ShellToolArgs =
            serde_json::from_value(json!({"command":"cat","stdin_open":true})).unwrap();
        let mut shell = control
            .shell
            .spawn(
                "failure-observer-shell".into(),
                root.path().into(),
                args,
                BTreeMap::new(),
            )
            .await
            .unwrap();
        control.live.test_shells().lock().await.insert(
            "failure-observer-shell".into(),
            LiveShell {
                run_id: "fixture-run-without-driver".into(),
                handle: shell.handle.clone(),
            },
        );
        // Model a recovery-wait background shell: no driver remains to stop it.
        // Hold the map so begin_shutdown's synchronous try_lock cannot stop it.
        let held = control.live.test_shells().lock().await;
        let failure_control = control.clone();
        let failure = tokio::spawn(async move {
            failure_control.fail_storage().await;
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !control.storage_failed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        failure.abort();
        let _ = failure.await;
        drop(held);
        let completion = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(done) = shell.completion.borrow().clone() {
                    break done;
                }
                shell.completion.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(completion.state, ShellState::Stopped);
        control.live.test_shells().lock().await.clear();
        assert_eq!(
            control.shutdown().await.unwrap_err().code,
            ErrorCode::SaveFailed
        );
        owner.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod ipc_admission_tests;

#[cfg(all(test, unix))]
mod shell_lifecycle_tests;

#[cfg(all(test, unix))]
mod progress_tests;
